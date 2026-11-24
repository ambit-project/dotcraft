//! Solution Explorer: solution → solution folders → projects → Dependencies and the project's
//! folders and files (read lazily from disk). The Files tab shows every file of the git
//! repository instead (or of the workspace folder outside git). Keyboard navigation like Rider:
//! ↑/↓ move, →/← expand/collapse, Enter or double-click opens, F2 renames, ⌫ deletes.

pub use rc_core::session::ExplorerView;
use rc_core::workspace::{FileNode, list_dir, list_dir_with};
use rc_dotnet::{ProjectInfo, Solution, SolutionItem};
use rc_kit::i18n::tr;
use rc_kit::{Ico, ico, icons, theme};
use egui::{Align2, FontId, Id, Key, Rect, Sense, Ui, pos2, vec2};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const ROW_H: f32 = 24.0;
const INDENT: f32 = 18.0;

/// What a node is; its key (`Node::key`) identifies it across frames and in the session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Solution,
    Scratches,
    /// The Files tab's root: the repository's work tree (or the workspace folder).
    FilesRoot(PathBuf),
    SolutionFolder { name: String, children: Vec<SolutionItem> },
    Project(PathBuf),
    Dependencies(PathBuf),
    Frameworks(PathBuf),
    Packages(PathBuf),
    ProjectRefs(PathBuf),
    Framework(String),
    Package { project: PathBuf, frameworks: Vec<String>, name: String, version: String },
    ProjectRef(PathBuf),
    Dir(PathBuf),
    File(PathBuf),
}

#[derive(Clone, Debug)]
pub struct Row {
    pub key: String,
    pub kind: NodeKind,
    pub depth: usize,
    pub label: String,
    pub secondary: String,
    pub icon: Ico,
    pub expandable: bool,
}

#[derive(Default)]
pub struct ExplorerState {
    pub expanded: HashSet<String>,
    pub selected: Option<String>,
    pub dir_cache: HashMap<PathBuf, Vec<FileNode>>,
    pub projects: HashMap<PathBuf, ProjectInfo>,
    /// Scroll the selected row into view on the next frame.
    pub reveal: bool,
    pub has_focus: bool,
    /// Open files with a single click (Settings › Editor).
    pub single_click: bool,
    /// Show All Files (Rider): bin, obj and the files of the solution folder that belong to
    /// no project, drawn dimmed.
    pub show_all: bool,
    /// Keys of the rows drawn dimmed (not part of the solution), rebuilt by `rows`.
    dimmed: HashSet<String>,
    /// Override used by isolated explorer tests; production uses the shared data directory.
    scratch_dir: Option<PathBuf>,
    /// Solution or Files tab (kept in the session).
    pub view: ExplorerView,
    /// The git work tree containing the workspace: the Files tab's root once discovered.
    files_root: Option<PathBuf>,
    /// Folder listings of the Files tab: never bin, obj or .git, whatever Show All Files says.
    files_cache: HashMap<PathBuf, Vec<FileNode>>,
}

/// What the user asked for in the explorer this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum ExplorerAction {
    Open(PathBuf),
    /// Single-click open: show the file but keep the focus in the explorer.
    Preview(PathBuf),
    /// Add › item in this folder.
    NewItem(PathBuf, crate::item_templates::ItemKind),
    Rename(PathBuf),
    Delete(PathBuf),
    Reveal(PathBuf),
    CopyPath(PathBuf),
    OpenInTerminal(PathBuf),
    ManagePackages(PathBuf),
    /// Initialize and edit the local user-secrets file for a C# project.
    ManageUserSecrets(PathBuf),
    ProjectProperties(PathBuf),
    /// Explore Assembly on a NuGet package: its assemblies for `framework` (or the first one).
    OpenPackageAssembly { project: PathBuf, framework: Option<String>, name: String, version: String },
    /// Explore Assemblies… on a target framework: pick from its shared framework folder.
    ExploreFramework(String),
    /// Entity Framework Core › operation, for this project.
    EfCore(PathBuf, crate::ui::ef_core::EfOp),
    /// Publish… this project with a folder publish profile.
    Publish(PathBuf),
    /// Migrate this solution to Central Package Management (NuGet window).
    MigrateToCpm,
    /// Add New Project to this solution.
    AddProject(PathBuf),
    /// Build, Rebuild, Clean or Restore this solution or project.
    Build(PathBuf, rc_dotnet::BuildAction),
    /// Run (or debug, when true) this executable project with its first run configuration.
    RunProject(PathBuf, bool),
    /// Run the tests of this solution or test project.
    RunTests(PathBuf),
    Collapse,
    LocateActive,
    /// Show All Files on/off (kept in the session).
    ToggleShowAll,
    /// Switch to the Solution or the Files tab.
    SetView(ExplorerView),
    /// Open a Markdown file showing only its preview.
    OpenPreview(PathBuf),
}

/// `.md` and `.markdown` files, which the editor can show as a preview.
pub fn is_markdown(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("md") || e.eq_ignore_ascii_case("markdown"))
}

impl ExplorerState {
    fn scratches_dir(&self) -> PathBuf {
        self.scratch_dir.clone().unwrap_or_else(rc_core::paths::scratches_dir)
    }

    pub fn invalidate(&mut self, path: &Path) {
        if let Some(parent) = path.parent() {
            self.dir_cache.remove(parent);
        }
        self.dir_cache.remove(path);
        if let Some(parent) = path.parent() {
            self.files_cache.remove(parent);
        }
        self.files_cache.remove(path);
        if path.extension().is_some_and(|e| e == "csproj") {
            self.projects.remove(path);
        }
    }

    fn children_of_dir(&mut self, dir: &Path) -> Vec<FileNode> {
        let all = self.show_all;
        self.dir_cache.entry(dir.to_path_buf()).or_insert_with(|| list_dir_with(dir, all)).clone()
    }

    pub fn set_show_all(&mut self, on: bool) {
        if self.show_all != on {
            self.show_all = on;
            self.dir_cache.clear();
        }
    }

    /// The git work tree around the workspace (None outside git); the Files tab starts there.
    pub fn set_files_root(&mut self, root: Option<PathBuf>) {
        if self.files_root != root {
            self.files_root = root;
            self.files_cache.clear();
        }
    }


    /// The Files tab's root for workspace `root`: the repository when it contains the workspace.
    fn files_top(&self, root: &Path) -> PathBuf {
        self.files_root.as_ref().filter(|r| root.starts_with(r)).cloned().unwrap_or_else(|| root.to_path_buf())
    }

    pub fn set_view(&mut self, view: ExplorerView) {
        self.view = view;
        // The first visit shows the repository's top level.
        if view == ExplorerView::Files && !self.expanded.iter().any(|k| k == "files" || k.starts_with("fdir:")) {
            self.expanded.insert("files".into());
        }
    }

    /// Collapse All: folds the current tab only.
    pub fn collapse_all(&mut self) {
        match self.view {
            ExplorerView::Solution => self.expanded.retain(|k| k == "sln" || k == "files" || k.starts_with("fdir:")),
            ExplorerView::Files => self.expanded.retain(|k| !k.starts_with("fdir:")),
        }
    }

    pub fn is_dimmed(&self, key: &str) -> bool {
        self.dimmed.contains(key)
    }

    fn project(&mut self, path: &Path) -> Option<ProjectInfo> {
        if !self.projects.contains_key(path) {
            let info = ProjectInfo::load(path).ok()?;
            self.projects.insert(path.to_path_buf(), info);
        }
        self.projects.get(path).cloned()
    }

    /// Expands the parents of `path` and selects it (Locate in explorer / "Always select
    /// opened file").
    pub fn locate(&mut self, path: &Path, solution: Option<&Solution>, root: &Path) {
        let scratches = self.scratches_dir();
        if self.view == ExplorerView::Files {
            let top = self.files_top(root);
            if path.starts_with(&top) && !path.starts_with(&scratches) {
                let mut keys = vec!["files".to_string()];
                let mut dirs = Vec::new();
                let mut parent = path.parent();
                while let Some(dir) = parent.filter(|dir| *dir != top && dir.starts_with(&top)) {
                    dirs.push(format!("fdir:{}", dir.display()));
                    parent = dir.parent();
                }
                keys.extend(dirs.into_iter().rev());
                self.expanded.extend(keys);
                self.selected = Some(format!("file:{}", path.display()));
                self.reveal = true;
                return;
            }
            // Scratches and files outside the repository are only in the Solution tab.
            self.view = ExplorerView::Solution;
        }
        if path.starts_with(&scratches) {
            let mut keys = vec!["scratches".to_string()];
            let mut dirs = Vec::new();
            let mut parent = path.parent();
            while let Some(dir) = parent.filter(|dir| *dir != scratches && dir.starts_with(&scratches)) {
                dirs.push(format!("dir:{}", dir.display()));
                parent = dir.parent();
            }
            keys.extend(dirs.into_iter().rev());
            self.expanded.extend(keys);
            self.selected = Some(format!("file:{}", path.display()));
            self.reveal = true;
            return;
        }
        let mut keys = vec!["sln".to_string()];
        if let Some(sln) = solution {
            // The project that contains the file, and the solution folders around it.
            let project = sln.projects().into_iter().filter(|p| p.path.parent().is_some_and(|d| path.starts_with(d))).max_by_key(|p| p.path.as_os_str().len()).cloned();
            if let Some(p) = project {
                fn find(items: &[SolutionItem], target: &Path, trail: &mut Vec<String>) -> bool {
                    for i in items {
                        match i {
                            SolutionItem::Project(p) if p.path == target => return true,
                            SolutionItem::Folder { name, children } => {
                                trail.push(format!("{}/{name}", trail.last().cloned().unwrap_or_default()));
                                if find(children, target, trail) {
                                    return true;
                                }
                                trail.pop();
                            }
                            _ => {}
                        }
                    }
                    false
                }
                let mut trail = Vec::new();
                find(&sln.items, &p.path, &mut trail);
                keys.extend(trail.into_iter().map(|t| format!("sf:{t}")));
                keys.push(format!("proj:{}", p.path.display()));
                let pdir = p.path.parent().unwrap_or(root).to_path_buf();
                let mut d = path.parent().map(Path::to_path_buf);
                let mut dirs = Vec::new();
                while let Some(cur) = d {
                    if cur == pdir || !cur.starts_with(&pdir) {
                        break;
                    }
                    dirs.push(format!("dir:{}", cur.display()));
                    d = cur.parent().map(Path::to_path_buf);
                }
                keys.extend(dirs.into_iter().rev());
            }
        } else {
            let mut d = path.parent().map(Path::to_path_buf);
            let mut dirs = Vec::new();
            while let Some(cur) = d {
                if !cur.starts_with(root) || cur == root {
                    break;
                }
                dirs.push(format!("dir:{}", cur.display()));
                d = cur.parent().map(Path::to_path_buf);
            }
            keys.extend(dirs.into_iter().rev());
        }
        self.expanded.extend(keys);
        self.selected = Some(format!("file:{}", path.display()));
        self.reveal = true;
    }

    /// The visible rows: a depth-first walk of the expanded nodes.
    pub fn rows(&mut self, solution: Option<&Solution>, root: &Path, root_name: &str) -> Vec<Row> {
        let mut rows = Vec::new();
        self.dimmed.clear();
        if self.view == ExplorerView::Files {
            self.push_files_root(&mut rows, root);
            return rows;
        }
        let root_row = Row {
            key: "sln".into(),
            kind: NodeKind::Solution,
            depth: 0,
            label: root_name.to_string(),
            secondary: solution.map_or_else(String::new, |s| {
                let n = s.projects().len();
                if n == 1 { tr("1 project").to_string() } else { rc_kit::i18n::trf("{n} projects", &[("n", &n)]) }
            }),
            icon: if solution.is_some() { ico::SOLUTION } else { ico::FOLDER },
            expandable: true,
        };
        rows.push(root_row);
        if self.expanded.contains("sln") {
            match solution {
                Some(sln) => {
                    let items = sln.items.clone();
                    self.push_items(&mut rows, &items, 1, "");
                    if self.show_all {
                        self.push_unattached(&mut rows, sln);
                    }
                }
                None => self.push_dir(&mut rows, root, 1, false),
            }
        }
        rows.push(Row {
            key: "scratches".into(),
            kind: NodeKind::Scratches,
            depth: 0,
            label: tr("Scratches").to_string(),
            secondary: String::new(),
            icon: ico::FOLDER,
            expandable: true,
        });
        if self.expanded.contains("scratches") {
            self.push_dir(&mut rows, &self.scratches_dir(), 1, false);
        }
        rows
    }

    /// The Files tab: the repository's folders and files, read lazily like a project's.
    fn push_files_root(&mut self, rows: &mut Vec<Row>, root: &Path) {
        let top = self.files_top(root);
        rows.push(Row {
            key: "files".into(),
            kind: NodeKind::FilesRoot(top.clone()),
            depth: 0,
            label: top.file_name().map_or_else(|| top.display().to_string(), |n| n.to_string_lossy().into_owned()),
            secondary: String::new(),
            icon: ico::FOLDER,
            expandable: true,
        });
        if self.expanded.contains("files") {
            self.push_files_dir(rows, &top, 1);
        }
    }

    fn push_files_dir(&mut self, rows: &mut Vec<Row>, dir: &Path, depth: usize) {
        let children = self.files_cache.entry(dir.to_path_buf()).or_insert_with(|| list_dir(dir)).clone();
        for node in children {
            if node.is_dir {
                let key = format!("fdir:{}", node.path.display());
                rows.push(Row { key: key.clone(), kind: NodeKind::Dir(node.path.clone()), depth, label: node.name.clone(), secondary: String::new(), icon: ico::FOLDER, expandable: true });
                if self.expanded.contains(&key) {
                    self.push_files_dir(rows, &node.path, depth + 1);
                }
            } else {
                rows.push(file_row(&node.path, depth));
            }
        }
    }

    /// Show All Files: what the solution folder holds besides the solution and its projects
    /// (README, .gitignore, scripts…), like Rider's unattached files.
    fn push_unattached(&mut self, rows: &mut Vec<Row>, sln: &Solution) {
        let Some(dir) = sln.path.parent() else { return };
        let project_dirs: Vec<PathBuf> = sln.projects().iter().filter_map(|p| p.path.parent().map(Path::to_path_buf)).collect();
        fn files(items: &[SolutionItem], out: &mut Vec<PathBuf>) {
            for i in items {
                match i {
                    SolutionItem::File(f) => out.push(f.clone()),
                    SolutionItem::Folder { children, .. } => files(children, out),
                    SolutionItem::Project(_) => {}
                }
            }
        }
        let mut listed = Vec::new();
        files(&sln.items, &mut listed);
        for c in self.children_of_dir(dir) {
            let attached = if c.is_dir { project_dirs.iter().any(|d| d.starts_with(&c.path)) } else { c.path == sln.path || listed.contains(&c.path) };
            if !attached {
                self.push_node(rows, &c, 1, true);
            }
        }
    }

    fn push_items(&mut self, rows: &mut Vec<Row>, items: &[SolutionItem], depth: usize, trail: &str) {
        for item in items {
            match item {
                SolutionItem::Folder { name, children } => {
                    let path = format!("{trail}/{name}");
                    let key = format!("sf:{path}");
                    rows.push(Row {
                        key: key.clone(),
                        kind: NodeKind::SolutionFolder { name: name.clone(), children: children.clone() },
                        depth,
                        label: name.clone(),
                        secondary: String::new(),
                        icon: ico::FOLDER,
                        expandable: !children.is_empty(),
                    });
                    if self.expanded.contains(&key) {
                        self.push_items(rows, children, depth + 1, &path);
                    }
                }
                SolutionItem::Project(p) => self.push_project(rows, &p.path, &p.name, depth),
                SolutionItem::File(f) => rows.push(file_row(f, depth)),
            }
        }
    }

    fn push_project(&mut self, rows: &mut Vec<Row>, path: &Path, name: &str, depth: usize) {
        let key = format!("proj:{}", path.display());
        let info = self.project(path);
        let exists = path.exists();
        rows.push(Row {
            key: key.clone(),
            kind: NodeKind::Project(path.to_path_buf()),
            depth,
            label: name.to_string(),
            secondary: if !exists { tr("(not found)").to_string() } else { info.as_ref().map(|i| i.target_frameworks.join(", ")).unwrap_or_default() },
            icon: if info.as_ref().is_some_and(|i| i.is_test) { ico::TEST_ROOT } else { ico::PROJECT },
            expandable: exists,
        });
        if !self.expanded.contains(&key) || !exists {
            return;
        }
        // Dependencies first, then the project folder's content (minus the project file).
        let dkey = format!("deps:{}", path.display());
        rows.push(Row {
            key: dkey.clone(),
            kind: NodeKind::Dependencies(path.to_path_buf()),
            depth: depth + 1,
            label: tr("Dependencies").to_string(),
            secondary: String::new(),
            icon: ico::LIBRARY,
            expandable: true,
        });
        if self.expanded.contains(&dkey)
            && let Some(info) = &info
        {
            let groups: [(String, &str, Ico, bool); 3] = [
                (format!("fw:{}", path.display()), tr("Frameworks"), ico::LIBRARY, !info.target_frameworks.is_empty()),
                (format!("pkgs:{}", path.display()), tr("Packages"), ico::PACKAGE, !info.packages.is_empty()),
                (format!("prefs:{}", path.display()), tr("Projects"), ico::PROJECT_GROUP, !info.project_refs.is_empty()),
            ];
            for (i, (gkey, label, icon, nonempty)) in groups.into_iter().enumerate() {
                if !nonempty {
                    continue;
                }
                let kind = match i {
                    0 => NodeKind::Frameworks(path.to_path_buf()),
                    1 => NodeKind::Packages(path.to_path_buf()),
                    _ => NodeKind::ProjectRefs(path.to_path_buf()),
                };
                rows.push(Row { key: gkey.clone(), kind, depth: depth + 2, label: label.to_string(), secondary: String::new(), icon, expandable: true });
                if !self.expanded.contains(&gkey) {
                    continue;
                }
                match i {
                    0 => {
                        for fw in &info.target_frameworks {
                            rows.push(Row { key: format!("{gkey}/{fw}"), kind: NodeKind::Framework(fw.clone()), depth: depth + 3, label: framework_label(fw), secondary: fw.clone(), icon: ico::LIBRARY, expandable: false });
                        }
                    }
                    1 => {
                        for p in &info.packages {
                            rows.push(Row {
                                key: format!("{gkey}/{}", p.name),
                                kind: NodeKind::Package { project: path.to_path_buf(), frameworks: info.target_frameworks.clone(), name: p.name.clone(), version: p.version.clone() },
                                depth: depth + 3,
                                label: p.name.clone(),
                                secondary: p.version.clone(),
                                icon: ico::PACKAGE,
                                expandable: false,
                            });
                        }
                    }
                    _ => {
                        for r in &info.project_refs {
                            let name = r.file_stem().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                            rows.push(Row { key: format!("{gkey}/{}", r.display()), kind: NodeKind::ProjectRef(r.clone()), depth: depth + 3, label: name, secondary: String::new(), icon: ico::PROJECT, expandable: false });
                        }
                    }
                }
            }
        }
        let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let children = self.children_of_dir(&dir);
        for c in children {
            if c.path == path {
                continue;
            }
            self.push_node(rows, &c, depth + 1, false);
        }
    }

    fn push_dir(&mut self, rows: &mut Vec<Row>, dir: &Path, depth: usize, dim: bool) {
        for c in self.children_of_dir(dir) {
            self.push_node(rows, &c, depth, dim);
        }
    }

    /// `dim`: the parent is not part of the solution, so neither is this node.
    fn push_node(&mut self, rows: &mut Vec<Row>, node: &FileNode, depth: usize, dim: bool) {
        let dim = dim || node.hidden;
        if node.is_dir {
            let key = format!("dir:{}", node.path.display());
            let special = node.name == "Properties";
            rows.push(Row {
                key: key.clone(),
                kind: NodeKind::Dir(node.path.clone()),
                depth,
                label: node.name.clone(),
                secondary: String::new(),
                icon: if special { ico::FILE_CONFIG } else { ico::FOLDER },
                expandable: true,
            });
            if dim {
                self.dimmed.insert(key.clone());
            }
            if self.expanded.contains(&key) {
                self.push_dir(rows, &node.path, depth + 1, dim);
            }
        } else {
            let row = file_row(&node.path, depth);
            if dim {
                self.dimmed.insert(row.key.clone());
            }
            rows.push(row);
        }
    }
}

fn file_row(path: &Path, depth: usize) -> Row {
    let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    Row { key: format!("file:{}", path.display()), kind: NodeKind::File(path.to_path_buf()), depth, icon: icons::for_file(&name), label: name, secondary: String::new(), expandable: false }
}

/// "net8.0" → ".NET 8.0".
fn framework_label(tfm: &str) -> String {
    if let Some(v) = tfm.strip_prefix("net").filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()) && v.contains('.')) {
        format!(".NET {v}")
    } else {
        tfm.to_string()
    }
}

/// Draws the explorer inside its island; returns the user's requests.
pub fn show(ui: &mut Ui, state: &mut ExplorerState, solution: Option<&Solution>, root: &Path, root_name: &str, active_file: Option<&Path>) -> Vec<ExplorerAction> {
    let p = rc_kit::pal(ui);
    let mut actions = Vec::new();
    // Header: title and actions (New UI tool window header).
    let header = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 36.0));
    ui.allocate_rect(header, Sense::hover());
    ui.painter().text(pos2(header.left() + 12.0, header.center().y), Align2::LEFT_CENTER, tr("Explorer"), rc_kit::theme::bold(13.0), p.text);
    let mut x = header.right() - 8.0;
    for (icon, tip, action) in [
        (ico::HIDE, tr("Hide"), None),
        (ico::COLLAPSE_ALL, tr("Collapse All"), Some(ExplorerAction::Collapse)),
        (ico::LOCATE, tr("Select Opened File"), Some(ExplorerAction::LocateActive)),
        (ico::SHOW, tr("Show All Files"), Some(ExplorerAction::ToggleShowAll)),
    ] {
        x -= 26.0;
        let r = Rect::from_min_size(pos2(x, header.center().y - 13.0), vec2(26.0, 26.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r));
        let enabled = !(action == Some(ExplorerAction::LocateActive) && active_file.is_none());
        let clicked = if action == Some(ExplorerAction::ToggleShowAll) {
            rc_kit::theme::icon_toggle(&mut child, icon, state.show_all, tip).clicked()
        } else {
            rc_kit::theme::icon_button(&mut child, icon, enabled, tip).clicked()
        };
        if clicked {
            match action {
                Some(a) => actions.push(a),
                None => ui.ctx().data_mut(|d| {
                    d.insert_temp(Id::new("explorer_hide"), true);
                }),
            }
        }
    }

    // Solution / Files tabs (Rider's Solution and File System views).
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.spacing_mut().item_spacing.x = 4.0;
        for (view, icon, label) in [(ExplorerView::Solution, if solution.is_some() { ico::SOLUTION } else { ico::FOLDER }, tr("Solution")), (ExplorerView::Files, ico::FOLDER, tr("Files"))] {
            if theme::tab(ui, state.view == view, Some(icon), label, false).0.clicked() && state.view != view {
                actions.push(ExplorerAction::SetView(view));
            }
        }
    });
    ui.add_space(4.0);

    let rows = state.rows(solution, root, root_name);
    let selected_index = state.selected.as_ref().and_then(|k| rows.iter().position(|r| &r.key == k));
    let tree_id = Id::new("explorer_tree");
    // The tree takes the keyboard focus: give it an AccessKit node, or the focused id is
    // missing from the accessibility tree.
    ui.interact(ui.available_rect_before_wrap(), tree_id, Sense::hover()).widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Explorer tree"));
    let mut open_row: Option<usize> = None;
    let mut toggle_row: Option<usize> = None;
    let mut select_row: Option<usize> = None;
    let mut context: Option<(usize, egui::Response)> = None;

    let scroll = egui::ScrollArea::both().id_salt("explorer_scroll").auto_shrink([false, false]);
    scroll.show_rows(ui, ROW_H, rows.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for i in range {
            let row = &rows[i];
            let width = ui.available_width().max(200.0);
            let (rect, resp) = ui.allocate_exact_size(vec2(width, ROW_H), Sense::click());
            let selected = selected_index == Some(i);
            if selected {
                let color = if state.has_focus { p.selection } else { p.selection_inactive };
                ui.painter().rect_filled(rect.shrink2(vec2(6.0, 0.0)), 4.0, color);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect.shrink2(vec2(6.0, 0.0)), 4.0, p.row_hover);
            }
            let x0 = rect.left() + 10.0 + row.depth as f32 * INDENT;
            let cy = rect.center().y;
            if row.expandable {
                let chevron = if state.expanded.contains(&row.key) { ico::CHEVRON_DOWN } else { ico::CHEVRON_RIGHT };
                let cr = Rect::from_center_size(pos2(x0 + 8.0, cy), vec2(16.0, 16.0));
                icons::paint(ui.painter(), cr, chevron, &p, true);
                if resp.clicked() && ui.input(|i| i.pointer.interact_pos()).is_some_and(|pt| pt.x < x0 + 18.0) {
                    toggle_row = Some(i);
                }
            }
            let ir = Rect::from_center_size(pos2(x0 + 26.0, cy), vec2(16.0, 16.0));
            icons::paint(ui.painter(), ir, row.icon, &p, true);
            let text_color = if state.is_dimmed(&row.key) { p.muted } else { p.text };
            let label = ui.painter().layout_no_wrap(row.label.clone(), FontId::proportional(13.0), text_color);
            let lx = x0 + 40.0;
            ui.painter().galley(pos2(lx, (cy - label.size().y / 2.0).round()), label.clone(), text_color);
            if !row.secondary.is_empty() {
                let sec = ui.painter().layout_no_wrap(row.secondary.clone(), FontId::proportional(12.0), p.muted);
                ui.painter().galley(pos2(lx + label.size().x + 8.0, (cy - sec.size().y / 2.0).round()), sec, p.muted);
            }
            if selected && state.reveal {
                ui.scroll_to_rect(rect, Some(egui::Align::Center));
                state.reveal = false;
            }
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, &row.label));
            if resp.clicked() || resp.secondary_clicked() {
                select_row = Some(i);
                ui.memory_mut(|m| m.request_focus(tree_id));
            }
            if resp.double_clicked() {
                open_row = Some(i);
            } else if resp.clicked()
                && state.single_click
                && let NodeKind::File(p) = &row.kind
            {
                actions.push(ExplorerAction::Preview(p.clone()));
            }
            if resp.secondary_clicked() {
                context = Some((i, resp.clone()));
            }
            let info = match &rows[i].kind {
                NodeKind::Project(p) => state.projects.get(p),
                _ => None,
            };
            let kind = ProjectMenu {
                ef: info.is_some_and(rc_dotnet::ef::uses_ef),
                executable: info.is_some_and(|i| i.is_executable() && !i.is_godot() && !i.is_test),
                test: info.is_some_and(|i| i.is_test),
            };
            resp.context_menu(|ui| {
                context_menu(ui, &rows[i], solution.map(|s| s.path.as_path()), kind, &mut actions);
            });
        }
        // Empty space below the rows clears focus to the tree too.
        let rest = ui.available_rect_before_wrap();
        if rest.height() > 0.0 {
            let r = ui.allocate_rect(rest, Sense::click());
            if r.clicked() {
                ui.memory_mut(|m| m.request_focus(tree_id));
            }
        }
    });
    let _ = context;

    // Keyboard.
    let has_focus = ui.memory(|m| m.has_focus(tree_id));
    state.has_focus = has_focus;
    if has_focus {
        ui.memory_mut(|m| m.set_focus_lock_filter(tree_id, egui::EventFilter { tab: false, horizontal_arrows: true, vertical_arrows: true, escape: false }));
        let cur = selected_index.unwrap_or(0);
        let pressed = |k: Key| ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, k));
        if pressed(Key::ArrowDown) {
            select_row = Some((cur + 1).min(rows.len().saturating_sub(1)));
            state.reveal = true;
        }
        if pressed(Key::ArrowUp) {
            select_row = Some(cur.saturating_sub(1));
            state.reveal = true;
        }
        if pressed(Key::ArrowRight)
            && let Some(r) = rows.get(cur) {
                if r.expandable && !state.expanded.contains(&r.key) {
                    toggle_row = Some(cur);
                } else if r.expandable {
                    select_row = Some((cur + 1).min(rows.len() - 1));
                }
            }
        if pressed(Key::ArrowLeft)
            && let Some(r) = rows.get(cur) {
                if r.expandable && state.expanded.contains(&r.key) {
                    toggle_row = Some(cur);
                } else if let Some(parent) = (0..cur).rev().find(|&j| rows[j].depth < r.depth) {
                    select_row = Some(parent);
                    state.reveal = true;
                }
            }
        if pressed(Key::Enter) {
            open_row = Some(cur);
        }
        if pressed(Key::F2)
            && let Some(path) = rows.get(cur).and_then(row_path)
        {
            actions.push(ExplorerAction::Rename(path));
        }
        if (pressed(Key::Delete) || ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::Backspace)))
            && let Some(path) = rows.get(cur).and_then(row_path)
        {
            actions.push(ExplorerAction::Delete(path));
        }
    }

    if let Some(i) = select_row {
        state.selected = rows.get(i).map(|r| r.key.clone());
    }
    if let Some(i) = toggle_row {
        let key = rows[i].key.clone();
        if !state.expanded.remove(&key) {
            state.expanded.insert(key);
        }
    }
    if let Some(i) = open_row {
        match &rows[i].kind {
            NodeKind::File(p) => actions.push(ExplorerAction::Open(p.clone())),
            _ if rows[i].expandable => {
                let key = rows[i].key.clone();
                if !state.expanded.remove(&key) {
                    state.expanded.insert(key);
                }
            }
            _ => {}
        }
    }
    actions
}

/// File system path of a row (for file actions).
fn row_path(r: &Row) -> Option<PathBuf> {
    match &r.kind {
        NodeKind::File(p) | NodeKind::Dir(p) | NodeKind::Project(p) => Some(p.clone()),
        _ => None,
    }
}

/// What a project's context menu offers besides the common items.
#[derive(Clone, Copy, Default)]
struct ProjectMenu {
    ef: bool,
    executable: bool,
    test: bool,
}

/// Build › items of a solution or project (Rider's Build, Rebuild, Clean, Restore).
fn build_items(ui: &mut Ui, target: &Path, solution: bool, actions: &mut Vec<ExplorerAction>) {
    use rc_dotnet::BuildAction;
    let items = if solution {
        [(tr("Build Solution"), ico::BUILD, BuildAction::Build), (tr("Rebuild Solution"), ico::REBUILD, BuildAction::Rebuild), (tr("Clean Solution"), ico::DELETE, BuildAction::Clean)]
    } else {
        [(tr("Build Project"), ico::BUILD, BuildAction::Build), (tr("Rebuild Project"), ico::REBUILD, BuildAction::Rebuild), (tr("Clean Project"), ico::DELETE, BuildAction::Clean)]
    };
    for (label, icon, action) in items.into_iter().chain([(tr("Restore NuGet Packages"), ico::DOWNLOAD, BuildAction::Restore)]) {
        if theme::menu_item(ui, Some(icon), label, false).clicked() {
            actions.push(ExplorerAction::Build(target.to_path_buf(), action));
            ui.close();
        }
    }
}

fn context_menu(ui: &mut Ui, row: &Row, solution: Option<&Path>, project: ProjectMenu, actions: &mut Vec<ExplorerAction>) {
    let ef = project.ef;
    ui.set_min_width(220.0);
    if matches!(&row.kind, NodeKind::Scratches) {
        if theme::menu_item(ui, Some(ico::NEW_FILE), tr("New Scratch File…"), false).clicked() {
            actions.push(ExplorerAction::NewItem(rc_core::paths::scratches_dir(), crate::item_templates::ItemKind::File));
            ui.close();
        }
        return;
    }
    if let (NodeKind::Solution, Some(sln)) = (&row.kind, solution) {
        build_items(ui, sln, true, actions);
        if theme::menu_item(ui, Some(ico::RUN_ALL), tr("Run All Tests"), false).clicked() {
            actions.push(ExplorerAction::RunTests(sln.to_path_buf()));
            ui.close();
        }
        ui.separator();
        if theme::menu_item(ui, Some(ico::ADD), tr("Add New Project…"), false).clicked() {
            actions.push(ExplorerAction::AddProject(sln.to_path_buf()));
            ui.close();
        }
        if theme::menu_item(ui, Some(ico::EDIT), tr("Edit Solution File"), false).clicked() {
            actions.push(ExplorerAction::Open(sln.to_path_buf()));
            ui.close();
        }
        // Only for solutions without a Directory.Packages.props.
        if sln.parent().and_then(rc_dotnet::cpm::find_central_file).is_none() && theme::menu_item(ui, Some(ico::PACKAGE), tr("Migrate to Central Package Management…"), false).clicked() {
            actions.push(ExplorerAction::MigrateToCpm);
            ui.close();
        }
        if theme::menu_item(ui, Some(ico::OPEN), if cfg!(target_os = "macos") { tr("Reveal in Finder") } else { tr("Open in Explorer") }, false).clicked() {
            actions.push(ExplorerAction::Reveal(sln.to_path_buf()));
            ui.close();
        }
        return;
    }
    if let NodeKind::Project(p) = &row.kind {
        build_items(ui, p, false, actions);
        if project.executable {
            if theme::menu_item(ui, Some(ico::RUN), tr("Run"), false).clicked() {
                actions.push(ExplorerAction::RunProject(p.clone(), false));
                ui.close();
            }
            if theme::menu_item(ui, Some(ico::DEBUG), tr("Debug"), false).clicked() {
                actions.push(ExplorerAction::RunProject(p.clone(), true));
                ui.close();
            }
        }
        if project.test && theme::menu_item(ui, Some(ico::RUN_ALL), tr("Run Tests"), false).clicked() {
            actions.push(ExplorerAction::RunTests(p.clone()));
            ui.close();
        }
        ui.separator();
    }
    if let NodeKind::Project(p) = &row.kind
        && theme::menu_item(ui, Some(ico::EDIT), tr("Edit Project File"), false).clicked()
    {
        actions.push(ExplorerAction::Open(p.clone()));
        ui.close();
    }
    if let NodeKind::Project(p) = &row.kind
        && p.extension().is_some_and(|extension| extension == "csproj")
        && theme::menu_item(ui, Some(ico::EDIT), tr("Manage User Secrets"), false).clicked()
    {
        actions.push(ExplorerAction::ManageUserSecrets(p.clone()));
        ui.close();
    }
    if let NodeKind::Project(p) = &row.kind
        && p.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("csproj"))
        && theme::menu_item(ui, Some(ico::SETTINGS), tr("Project Properties…"), false).clicked()
    {
        actions.push(ExplorerAction::ProjectProperties(p.clone()));
        ui.close();
    }
    let dir_of = |p: &Path, is_dir: bool| if is_dir { p.to_path_buf() } else { p.parent().unwrap_or(p).to_path_buf() };
    if let NodeKind::Project(p) | NodeKind::Packages(p) | NodeKind::Dependencies(p) = &row.kind
        && theme::menu_item(ui, Some(ico::PACKAGE), tr("Manage NuGet Packages…"), false).clicked()
    {
        actions.push(ExplorerAction::ManagePackages(p.clone()));
        ui.close();
    }
    if let NodeKind::Package { project, frameworks, name, version } = &row.kind {
        if frameworks.len() > 1 {
            ui.menu_button(tr("Explore Assembly"), |ui| {
                ui.set_min_width(160.0);
                for framework in frameworks {
                    if theme::menu_item(ui, Some(ico::LIBRARY), &framework_label(framework), false).clicked() {
                        actions.push(ExplorerAction::OpenPackageAssembly { project: project.clone(), framework: Some(framework.clone()), name: name.clone(), version: version.clone() });
                        ui.close();
                    }
                }
            });
        } else if theme::menu_item(ui, Some(ico::LIBRARY), tr("Explore Assembly"), false).clicked() {
            actions.push(ExplorerAction::OpenPackageAssembly { project: project.clone(), framework: None, name: name.clone(), version: version.clone() });
            ui.close();
        }
        return;
    }
    if let NodeKind::Framework(tfm) = &row.kind {
        if theme::menu_item(ui, Some(ico::LIBRARY), tr("Explore Assemblies…"), false).clicked() {
            actions.push(ExplorerAction::ExploreFramework(tfm.clone()));
            ui.close();
        }
        return;
    }
    // Entity Framework Core › (Rider), for projects that reference EF Core.
    if let (NodeKind::Project(p), true) = (&row.kind, ef) {
        ui.menu_button(tr("Entity Framework Core"), |ui| {
            ui.set_min_width(220.0);
            for op in crate::ui::ef_core::EfOp::ALL {
                if theme::menu_item(ui, Some(op.icon()), &format!("{}…", tr(op.title())), false).clicked() {
                    actions.push(ExplorerAction::EfCore(p.clone(), op));
                    ui.close();
                }
            }
        });
    }
    if let NodeKind::Project(p) = &row.kind
        && theme::menu_item(ui, Some(ico::BUILD), tr("Publish…"), false).clicked()
    {
        actions.push(ExplorerAction::Publish(p.clone()));
        ui.close();
    }
    let (path, is_dir) = match &row.kind {
        NodeKind::File(p) => (p.clone(), false),
        NodeKind::Dir(p) | NodeKind::FilesRoot(p) => (p.clone(), true),
        NodeKind::Project(p) => (p.clone(), false),
        NodeKind::Packages(_) | NodeKind::Dependencies(_) => return,
        _ => {
            ui.label(egui::RichText::new(tr("No actions for this item")).color(rc_kit::pal(ui).muted));
            return;
        }
    };
    let target_dir = match &row.kind {
        NodeKind::Project(p) => p.parent().unwrap_or(p).to_path_buf(),
        _ => dir_of(&path, is_dir),
    };
    if let NodeKind::File(p) = &row.kind
        && theme::menu_item(ui, Some(ico::OPEN), tr("Open"), false).clicked()
    {
        actions.push(ExplorerAction::Open(p.clone()));
        ui.close();
    }
    if let NodeKind::File(p) = &row.kind
        && is_markdown(p)
        && theme::menu_item(ui, Some(ico::PREVIEW), tr("Open Preview"), false).clicked()
    {
        actions.push(ExplorerAction::OpenPreview(p.clone()));
        ui.close();
    }
    // Add › (Rider): types, controllers, Razor, tests, files and folders.
    ui.menu_button(tr("Add"), |ui| {
        let id = ui.id().with("new_item_filter");
        let mut filter = ui.ctx().data(|data| data.get_temp::<String>(id).unwrap_or_default());
        new_item_menu(ui, &target_dir, &mut filter, actions);
        ui.ctx().data_mut(|data| data.insert_temp(id, filter));
    });
    ui.separator();
    // The Files tab's root is the repository itself: no rename or delete from here.
    if !matches!(&row.kind, NodeKind::FilesRoot(_)) {
        if theme::menu_item(ui, Some(ico::EDIT), tr("Rename…"), false).clicked() {
            actions.push(ExplorerAction::Rename(path.clone()));
            ui.close();
        }
        if theme::menu_item(ui, Some(ico::DELETE), tr("Delete…"), false).clicked() {
            actions.push(ExplorerAction::Delete(path.clone()));
            ui.close();
        }
    }
    ui.separator();
    if theme::menu_item(ui, Some(ico::COPY), tr("Copy Path"), false).clicked() {
        actions.push(ExplorerAction::CopyPath(path.clone()));
        ui.close();
    }
    let reveal = if cfg!(target_os = "macos") { tr("Reveal in Finder") } else { tr("Open in Explorer") };
    if theme::menu_item(ui, Some(ico::OPEN), reveal, false).clicked() {
        actions.push(ExplorerAction::Reveal(path.clone()));
        ui.close();
    }
    if theme::menu_item(ui, Some(ico::FILE_SHELL), tr("Open in Terminal"), false).clicked() {
        actions.push(ExplorerAction::OpenInTerminal(target_dir));
        ui.close();
    }
}

/// Searchable Add menu adapted from CrabCraft commits 95e6a42 and 2356dd9.
/// Copyright (c) 2026 the CrabCraft contributors.
fn new_item_menu(ui: &mut Ui, target: &Path, filter: &mut String, actions: &mut Vec<ExplorerAction>) {
    ui.set_min_width(260.0);
    ui.set_max_width(320.0);
    let pass = ui.ctx().cumulative_pass_nr();
    let focus_id = ui.id().with("new_item_search_opened");
    let opened = ui.ctx().data_mut(|data| {
        let (last, mut opened) = data.get_temp::<(u64, u64)>(focus_id).unwrap_or((pass, pass));
        if last.saturating_add(1) < pass {
            filter.clear();
            opened = pass;
        }
        data.insert_temp(focus_id, (pass, opened));
        opened
    });
    let field = ui.add(egui::TextEdit::singleline(filter).hint_text(tr("Search file type")).desired_width(260.0));
    if pass <= opened.saturating_add(1) {
        field.request_focus();
    }
    let enter = (field.has_focus() || field.lost_focus()) && ui.input(|input| input.key_pressed(Key::Enter));
    ui.separator();
    let query = filter.trim().to_lowercase();
    let mut first = None;
    let mut selected = None;
    let height = (ui.ctx().content_rect().bottom() - ui.cursor().top() - 16.0).clamp(80.0, 360.0);
    egui::ScrollArea::vertical().max_height(height).show(ui, |ui| {
        for entry in crate::item_templates::ItemKind::MENU {
            if let Some(kind) = entry {
                let title = tr(kind.title());
                if !title.to_lowercase().contains(&query) {
                    continue;
                }
                if first.is_none() {
                    first = Some(*kind);
                }
                let label = format!("{title}…");
                if theme::menu_item(ui, Some(kind.icon()), &label, false).clicked() {
                    selected = Some(*kind);
                }
            } else if query.is_empty() {
                ui.separator();
            }
        }
        if first.is_none() {
            ui.label(tr("No results"));
        }
    });
    if let Some(kind) = selected.or_else(|| enter.then_some(first).flatten()) {
        actions.push(ExplorerAction::NewItem(target.to_path_buf(), kind));
        ui.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratches_are_visible_outside_the_workspace_and_can_be_relocated() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let scratches = dir.path().join("scratches");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(scratches.join("notes")).unwrap();
        let scratch = scratches.join("notes/example.http");
        std::fs::write(&scratch, "GET https://example.org\n").unwrap();

        let mut explorer = ExplorerState { scratch_dir: Some(scratches), ..Default::default() };
        explorer.expanded.insert("sln".into());
        explorer.locate(&scratch, None, &workspace);
        let rows = explorer.rows(None, &workspace, "workspace");
        assert!(rows.iter().any(|row| row.key == "scratches" && row.depth == 0));
        assert!(rows.iter().any(|row| row.kind == NodeKind::File(scratch.clone()) && row.depth == 2));
        assert_eq!(explorer.selected, Some(format!("file:{}", scratch.display())));
    }

    #[test]
    fn show_all_files_adds_bin_obj_and_unattached_files_dimmed() {
        let dir = tempfile::Builder::new().prefix("dotcraft-explorer").tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let sln = root.join("App.sln");
        std::fs::write(&sln, "Microsoft Visual Studio Solution File, Format Version 12.00\nProject(\"{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}\") = \"App\", \"App\\App.csproj\", \"{790D8657-5CBB-446D-930E-78AA39237123}\"\nEndProject\n").unwrap();
        std::fs::create_dir_all(root.join("App/bin/Debug")).unwrap();
        std::fs::create_dir_all(root.join("App/obj")).unwrap();
        std::fs::write(root.join("App/App.csproj"), "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>").unwrap();
        std::fs::write(root.join("App/Program.cs"), "").unwrap();
        std::fs::write(root.join("README.md"), "").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let solution = Solution::load(&sln).unwrap();
        let mut state = ExplorerState::default();
        state.expanded.extend(["sln".to_string(), format!("proj:{}", root.join("App/App.csproj").display())]);
        let labels = |s: &mut ExplorerState| s.rows(Some(&solution), &root, "App").into_iter().map(|r| r.label).collect::<Vec<_>>();
        let off = labels(&mut state);
        assert!(off.contains(&"Program.cs".to_string()));
        assert!(!off.iter().any(|l| l == "bin" || l == "obj" || l == "README.md"));

        state.set_show_all(true);
        let on = labels(&mut state);
        for name in ["bin", "obj", "README.md", "Program.cs"] {
            assert!(on.contains(&name.to_string()), "{name} missing in {on:?}");
        }
        assert!(!on.iter().any(|l| l == ".git" || l == "App.sln"));
        assert!(state.is_dimmed(&format!("dir:{}", root.join("App/bin").display())));
        assert!(state.is_dimmed(&format!("file:{}", root.join("README.md").display())));
        assert!(!state.is_dimmed(&format!("file:{}", root.join("App/Program.cs").display())));
    }

    #[test]
    fn unity_folder_hides_generated_folders_until_show_all_files_dims_them() {
        let dir = tempfile::Builder::new().prefix("dotcraft-unity-explorer").tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        for d in ["Assets/Scripts", "Assets/Library", "ProjectSettings", "Packages", "Library/PackageCache", "Temp", "Logs"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("ProjectSettings/ProjectVersion.txt"), "m_EditorVersion: 2022.3.10f1\n").unwrap();
        std::fs::write(root.join("Assets/Scripts/Player.cs"), "").unwrap();
        let ws = rc_core::Workspace::open(&root).unwrap();
        assert_eq!(ws.kind, rc_core::WorkspaceKind::Folder);
        let mut state = ExplorerState::default();
        state.expanded.extend(["sln".to_string(), format!("dir:{}", root.join("Assets").display())]);
        let labels = |s: &mut ExplorerState| s.rows(None, &root, &ws.name()).into_iter().map(|r| r.label).collect::<Vec<_>>();
        let off = labels(&mut state);
        for name in ["Assets", "Scripts", "ProjectSettings", "Packages"] {
            assert!(off.contains(&name.to_string()), "{name} missing in {off:?}");
        }
        // Only the root's generated folders are hidden: `Assets/Library` is project content.
        assert_eq!(off.iter().filter(|l| *l == "Library").count(), 1, "{off:?}");
        assert!(!off.iter().any(|l| l == "Temp" || l == "Logs"));

        state.set_show_all(true);
        let on = labels(&mut state);
        for name in ["Temp", "Logs"] {
            assert!(on.contains(&name.to_string()), "{name} missing in {on:?}");
        }
        assert!(state.is_dimmed(&format!("dir:{}", root.join("Library").display())));
        assert!(state.is_dimmed(&format!("dir:{}", root.join("Temp").display())));
        assert!(!state.is_dimmed(&format!("dir:{}", root.join("Assets/Library").display())));
        assert!(!state.is_dimmed(&format!("dir:{}", root.join("Assets").display())));
    }

    /// A repository whose solution lies in `src/`, with files that belong to no project.
    fn repository() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::Builder::new().prefix("dotcraft-files-tab").tempdir().unwrap();
        let repo = dunce::canonicalize(dir.path()).unwrap();
        let src = repo.join("src");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(src.join("App/bin/Debug")).unwrap();
        std::fs::create_dir_all(src.join("App/obj")).unwrap();
        std::fs::create_dir_all(repo.join("scripts")).unwrap();
        std::fs::create_dir_all(repo.join("docs/guide")).unwrap();
        std::fs::write(src.join("App.sln"), "Microsoft Visual Studio Solution File, Format Version 12.00\nProject(\"{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}\") = \"App\", \"App\\App.csproj\", \"{790D8657-5CBB-446D-930E-78AA39237123}\"\nEndProject\n").unwrap();
        std::fs::write(src.join("App/App.csproj"), "<Project Sdk=\"Microsoft.NET.Sdk\"><PropertyGroup><TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>").unwrap();
        std::fs::write(src.join("App/Program.cs"), "").unwrap();
        std::fs::write(src.join("notes.txt"), "").unwrap();
        std::fs::write(repo.join("README.md"), "").unwrap();
        std::fs::write(repo.join("scripts/a.sh"), "").unwrap();
        std::fs::write(repo.join("docs/guide/x.md"), "").unwrap();
        (dir, repo, src)
    }

    #[test]
    fn files_tab_lists_the_whole_repository_without_git_bin_and_obj() {
        let (_dir, repo, src) = repository();
        let solution = Solution::load(&src.join("App.sln")).unwrap();
        let mut state = ExplorerState::default();
        state.set_files_root(Some(repo.clone()));
        state.expanded.insert("sln".into());
        state.set_view(ExplorerView::Files);
        let dirs = ["scripts", "src", "src/App", "src/App/bin", "src/App/obj"];
        state.expanded.extend(dirs.iter().map(|d| format!("fdir:{}", repo.join(d).display())));
        let rows = |s: &mut ExplorerState| s.rows(Some(&solution), &src, "App");

        let files = rows(&mut state);
        assert_eq!(files[0].kind, NodeKind::FilesRoot(repo.clone()), "the tab starts at the repository, not at src/");
        let labels: Vec<_> = files.iter().map(|r| r.label.as_str()).collect();
        for name in ["docs", "scripts", "a.sh", "src", "App", "App.csproj", "Program.cs", "App.sln", "notes.txt", "README.md"] {
            assert!(labels.contains(&name), "{name} missing in {labels:?}");
        }
        assert!(!labels.iter().any(|l| ["bin", "obj", ".git", "Scratches"].contains(l)), "{labels:?}");
        let csproj = files.iter().find(|r| r.label == "App.csproj").unwrap();
        assert_eq!(csproj.kind, NodeKind::File(src.join("App/App.csproj")), "a project is a plain file here");
        assert!(files.iter().all(|r| !state.is_dimmed(&r.key)));

        state.set_show_all(true);
        let all: Vec<_> = rows(&mut state).into_iter().map(|r| r.label).collect();
        assert!(!all.iter().any(|l| l == "bin" || l == "obj" || l == ".git"), "Show All Files only applies to the Solution tab: {all:?}");

        // The Solution tab is unchanged, with its expanded nodes kept apart from the Files tab's.
        state.set_show_all(false);
        state.set_view(ExplorerView::Solution);
        let solution_rows: Vec<_> = rows(&mut state).into_iter().map(|r| r.label).collect();
        assert_eq!(solution_rows, ["App", "App", "Scratches"]);
    }

    #[test]
    fn files_tab_outside_git_starts_at_the_workspace() {
        let (_dir, _repo, src) = repository();
        let mut state = ExplorerState::default();
        state.set_view(ExplorerView::Files);
        let rows = state.rows(None, &src, "src");
        assert_eq!(rows[0].kind, NodeKind::FilesRoot(src.clone()));
        assert_eq!(rows[0].label, "src");
        assert!(rows.iter().any(|r| r.label == "notes.txt") && !rows.iter().any(|r| r.label == "README.md"));
    }

    #[test]
    fn locate_in_the_files_tab_expands_the_folders_of_a_file_outside_the_projects() {
        let (_dir, repo, src) = repository();
        let solution = Solution::load(&src.join("App.sln")).unwrap();
        let mut state = ExplorerState { scratch_dir: Some(repo.join("scratches")), ..Default::default() };
        state.set_files_root(Some(repo.clone()));
        state.set_view(ExplorerView::Files);
        state.expanded.remove("files");
        let doc = repo.join("docs/guide/x.md");
        state.locate(&doc, Some(&solution), &src);
        assert_eq!(state.view, ExplorerView::Files);
        let rows = state.rows(Some(&solution), &src, "App");
        let row = rows.iter().find(|r| r.kind == NodeKind::File(doc.clone())).expect("located file is visible");
        assert_eq!(row.depth, 3);
        assert_eq!(state.selected, Some(row.key.clone()));

        // A scratch file is only in the Solution tab.
        let scratch = repo.join("scratches/a.http");
        std::fs::create_dir_all(scratch.parent().unwrap()).unwrap();
        std::fs::write(&scratch, "").unwrap();
        state.locate(&scratch, Some(&solution), &src);
        assert_eq!(state.view, ExplorerView::Solution);
    }

    #[test]
    fn collapse_all_folds_only_the_current_tab() {
        let mut state = ExplorerState::default();
        state.expanded.extend(["sln", "sf:/src", "proj:/a.csproj", "files", "fdir:/r/docs"].map(String::from));
        state.collapse_all();
        let mut left: Vec<_> = state.expanded.iter().cloned().collect();
        left.sort();
        assert_eq!(left, ["fdir:/r/docs", "files", "sln"]);
        state.expanded.extend(["sf:/src".to_string()]);
        state.view = ExplorerView::Files;
        state.collapse_all();
        let mut left: Vec<_> = state.expanded.iter().cloned().collect();
        left.sort();
        assert_eq!(left, ["files", "sf:/src", "sln"]);
    }

    #[test]
    fn files_tab_refreshes_after_changes_above_the_solution_folder() {
        let _lock = crate::screens::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let (_dir, repo, src) = repository();
        std::fs::remove_dir_all(repo.join(".git")).unwrap();
        assert!(std::process::Command::new("git").args(["init", "-q"]).current_dir(&repo).status().unwrap().success());
        let mut app = crate::screens::test_app(home.path(), true);
        app.settings.roslyn_path = Some("/nonexistent".into());
        app.open_workspace(&src.join("App.sln"));
        app.explorer.set_view(ExplorerView::Files);
        app.explorer.expanded.insert(format!("fdir:{}", repo.join("scripts").display()));
        let mut h = crate::screens::harness(app, egui::vec2(1280.0, 800.0));
        let has = |h: &mut egui_kittest::Harness<'static, crate::app::DotCraftApp>, name: &str| {
            let app = h.state_mut();
            let w = app.workspace.as_ref().unwrap();
            let (solution, root) = (w.solution.clone(), w.ws.root.clone());
            app.explorer.rows(solution.as_ref(), &root, "App").iter().any(|r| r.label == name)
        };
        let wait = |h: &mut egui_kittest::Harness<'static, crate::app::DotCraftApp>, name: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while !has(h, name) {
                assert!(std::time::Instant::now() < deadline, "{name} never appeared in the Files tab");
                h.run_steps(1);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        wait(&mut h, "a.sh");
        std::fs::write(repo.join("scripts/b.sh"), "").unwrap();
        wait(&mut h, "b.sh");
    }

    #[test]
    fn markdown_opens_as_a_preview_only() {
        let _lock = crate::screens::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let readme = home.path().join("README.md");
        std::fs::write(&readme, "# Title\n").unwrap();
        let mut app = crate::screens::test_app(home.path(), true);
        let ctx = egui::Context::default();
        app.handle_explorer(&ctx, vec![ExplorerAction::Open(readme.clone())]);
        assert_eq!(app.docs.active_doc().unwrap().md_view, Some(crate::docs::MdView::Split));
        app.handle_explorer(&ctx, vec![ExplorerAction::OpenPreview(readme.clone())]);
        assert_eq!(app.docs.list.len(), 1, "an open file switches to its preview");
        assert_eq!(app.docs.active_doc().unwrap().md_view, Some(crate::docs::MdView::Preview));
    }
}

#[cfg(test)]
mod template_picker_tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};

    fn picker() -> Harness<'static, (String, Vec<ExplorerAction>)> {
        Harness::new_ui_state(|ui, state: &mut (String, Vec<ExplorerAction>)| {
            let button = ui.button("Add template");
            egui::Popup::menu(&button).show(|ui| new_item_menu(ui, Path::new("/project/src"), &mut state.0, &mut state.1));
        }, (String::new(), Vec::new()))
    }

    #[test]
    fn search_focuses_and_enter_selects_a_dotnet_template() {
        let mut harness = picker();
        harness.get_by_label("Add template").click();
        harness.run_steps(3);
        harness.input_mut().events.push(egui::Event::Text("  json file  ".into()));
        harness.run_steps(2);
        assert_eq!(harness.state().0, "  json file  ");
        assert!(harness.query_by_label("Class…").is_none());
        assert!(harness.query_by_label("JSON File…").is_some());
        harness.input_mut().events.push(egui::Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
        harness.run_steps(2);
        assert_eq!(harness.state().1, [ExplorerAction::NewItem(PathBuf::from("/project/src"), crate::item_templates::ItemKind::Json)]);
        harness.get_by_label("Add template").click();
        harness.run_steps(3);
        assert!(harness.state().0.is_empty(), "the next Add menu should start with an empty query");
        assert!(harness.query_by_label("Class…").is_some());
    }

    #[test]
    fn empty_search_explains_result_and_enter_creates_nothing() {
        let mut harness = picker();
        harness.get_by_label("Add template").click();
        harness.run_steps(3);
        harness.input_mut().events.push(egui::Event::Text("  no-such-template  ".into()));
        harness.run_steps(2);
        assert!(harness.query_by_label(tr("No results")).is_some());
        harness.input_mut().events.push(egui::Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
        harness.run_steps(2);
        assert!(harness.state().1.is_empty());
    }
}

#[cfg(test)]
mod project_menu_tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};

    fn menu(kind: NodeKind, project: ProjectMenu) -> Harness<'static, Vec<ExplorerAction>> {
        let row = Row { key: "row".into(), kind, depth: 0, label: "row".into(), secondary: String::new(), icon: ico::PROJECT, expandable: false };
        Harness::new_ui_state(move |ui, actions: &mut Vec<ExplorerAction>| context_menu(ui, &row, Some(Path::new("/src/App.slnx")), project, actions), Vec::new())
    }

    #[test]
    fn files_in_the_files_tab_have_the_file_menu_and_markdown_opens_a_preview() {
        let mut h = menu(NodeKind::File(PathBuf::from("/src/App/App.csproj")), ProjectMenu::default());
        h.run();
        h.get_by_label(tr("Rename…"));
        assert!(h.query_by_label(tr("Build Project")).is_none() && h.query_by_label(tr("Run Tests")).is_none() && h.query_by_label(tr("Open Preview")).is_none());

        let readme = PathBuf::from("/repo/docs/README.md");
        let mut h = menu(NodeKind::File(readme.clone()), ProjectMenu::default());
        h.run();
        h.get_by_label(tr("Open Preview")).click();
        h.run();
        assert_eq!(h.state(), &[ExplorerAction::OpenPreview(readme)]);

        let mut h = menu(NodeKind::FilesRoot(PathBuf::from("/repo")), ProjectMenu::default());
        h.run();
        h.get_by_label(tr("Copy Path"));
        assert!(h.query_by_label(tr("Rename…")).is_none() && h.query_by_label(tr("Delete…")).is_none());
    }

    #[test]
    fn solution_menu_builds_and_tests_the_solution() {
        let mut h = menu(NodeKind::Solution, ProjectMenu::default());
        h.run();
        for label in ["Build Solution", "Rebuild Solution", "Clean Solution", "Restore NuGet Packages", "Run All Tests"] {
            h.get_by_label(tr(label));
        }
        h.get_by_label(tr("Rebuild Solution")).click();
        h.run();
        h.get_by_label(tr("Run All Tests")).click();
        h.run();
        let sln = PathBuf::from("/src/App.slnx");
        assert_eq!(h.state(), &[ExplorerAction::Build(sln.clone(), rc_dotnet::BuildAction::Rebuild), ExplorerAction::RunTests(sln)]);
    }

    #[test]
    fn api_and_worker_projects_build_run_and_debug() {
        let api = PathBuf::from("/src/Api/Api.csproj");
        let mut h = menu(NodeKind::Project(api.clone()), ProjectMenu { executable: true, ..Default::default() });
        h.run();
        assert!(h.query_by_label(tr("Run Tests")).is_none());
        h.get_by_label(tr("Build Project")).click();
        h.run();
        h.get_by_label(tr("Debug")).click();
        h.run();
        assert_eq!(h.state(), &[ExplorerAction::Build(api.clone(), rc_dotnet::BuildAction::Build), ExplorerAction::RunProject(api, true)]);
    }

    #[test]
    fn libraries_build_without_run_and_test_projects_run_tests() {
        let lib = PathBuf::from("/src/Lib/Lib.csproj");
        let mut h = menu(NodeKind::Project(lib.clone()), ProjectMenu::default());
        h.run();
        h.get_by_label(tr("Clean Project"));
        assert!(h.query_by_label(tr("Run")).is_none() && h.query_by_label(tr("Debug")).is_none() && h.query_by_label(tr("Run Tests")).is_none());

        let tests = PathBuf::from("/src/Lib.Tests/Lib.Tests.csproj");
        let mut h = menu(NodeKind::Project(tests.clone()), ProjectMenu { test: true, ..Default::default() });
        h.run();
        h.get_by_label(tr("Run Tests")).click();
        h.run();
        assert_eq!(h.state(), &[ExplorerAction::RunTests(tests)]);
    }
}
