//! PostgreSQL files (`.sql`, `.pgsql`, `.psql`) without a database: syntax and PL/pgSQL errors
//! in the editor and the Problems window, a hover with an object's signature, columns and
//! changeset, Go to Declaration (⌘-click) and Find Usages across the workspace's SQL files.
//!
//! PostgreSQL 18's own parser reads the files ([`rc_sql`], modelled on PG Studio). The workspace
//! is indexed on a background thread when it opens and again for the SQL files that change on
//! disk; open files are analysed on the UI thread after typing pauses and take the place of their
//! saved text in the index.

use crate::docs::{Doc, Docs};
use crate::insight::Target;
use rc_core::{Inbox, Runtime, Waker};
use rc_editor::{EditorEvent, Hover, Severity};
use rc_kit::i18n::{tr, trf};
use rc_kit::ico;
use rc_sql::{Definition, FileAnalysis, Hit, Index, ObjKind};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const ANALYZE_DELAY: Duration = Duration::from_millis(250);
const SCAN_DELAY: Duration = Duration::from_millis(400);
const MAX_FILES: usize = 20_000;
const MAX_BYTES: u64 = 4 << 20;

pub fn is_sql(d: &Doc) -> bool {
    d.editor.lang() == rc_syntax::Lang::Sql
}

/// What the rest of the UI must do.
#[derive(Debug, PartialEq)]
pub enum SqlOutput {
    /// Open a place (0-based line and char column).
    Open { path: PathBuf, line: usize, col: usize },
    Usages { title: String, targets: Vec<Target> },
    Message(String),
}

enum Event {
    /// Analyses of files on disk (None: the file is gone); `full` replaces everything.
    Scanned { generation: u64, full: bool, files: Vec<(PathBuf, Option<Arc<FileAnalysis>>)> },
    Tick,
}

struct DocState {
    path: PathBuf,
    version: Option<u64>,
    seen: (u64, Instant),
    analysis: Arc<FileAnalysis>,
    diags: Vec<rc_editor::Diagnostic>,
}

pub struct SqlInsight {
    inbox: Inbox<Event>,
    root: Option<PathBuf>,
    /// Analyses of the workspace's SQL files as saved.
    disk: HashMap<PathBuf, Arc<FileAnalysis>>,
    scan_gen: u64,
    scanning: bool,
    scan_due: Option<Instant>,
    /// Files to analyse again at `scan_due`; None with a due time: the whole workspace.
    changed: Option<Vec<PathBuf>>,
    states: HashMap<u64, DocState>,
    published: HashMap<PathBuf, Vec<rc_editor::Diagnostic>>,
    problems_dirty: bool,
    /// Built on demand from `disk` and the open files; dropped on every change.
    index: std::cell::RefCell<Option<Arc<Index>>>,
    /// Places offered by the open hover's buttons.
    hover_targets: Vec<(PathBuf, usize)>,
}

/// Every SQL file under `root` (outside build and tool folders).
fn sql_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let Ok(ty) = e.file_type() else { continue };
            let name = e.file_name().to_string_lossy().into_owned();
            if ty.is_dir() {
                if !rc_core::workspace::is_hidden_in(&dir, &name, true) && name != "target" {
                    stack.push(e.path());
                }
            } else if ty.is_file() && rc_sql::is_sql_path(&e.path()) && out.len() < MAX_FILES {
                out.push(e.path());
            }
        }
    }
    out.sort();
    out
}

fn analyze_file(path: &Path) -> Option<Arc<FileAnalysis>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return None;
    }
    Some(Arc::new(rc_sql::analyze(&std::fs::read_to_string(path).ok()?)))
}

/// Analyses `paths` on a few threads.
fn analyze_all(paths: Vec<PathBuf>) -> Vec<(PathBuf, Option<Arc<FileAnalysis>>)> {
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(1, 4);
    let chunk = paths.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = paths.chunks(chunk).map(|part| s.spawn(move || part.iter().map(|p| (p.clone(), analyze_file(p))).collect::<Vec<_>>())).collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

fn code(s: &str) -> String {
    format!("`{s}`")
}

/// 0-based line and char column of byte `offset` in `text`.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    (line, text[line_start..offset].chars().count())
}

fn char_of(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

impl SqlInsight {
    pub fn new(waker: Waker) -> Self {
        SqlInsight {
            inbox: Inbox::new(waker),
            root: None,
            disk: HashMap::new(),
            scan_gen: 0,
            scanning: false,
            scan_due: None,
            changed: None,
            states: HashMap::new(),
            published: HashMap::new(),
            problems_dirty: false,
            index: Default::default(),
            hover_targets: Vec::new(),
        }
    }

    /// A workspace was opened: its SQL files are indexed.
    pub fn set_workspace(&mut self, root: &Path) {
        self.root = Some(root.to_path_buf());
        self.disk.clear();
        self.index.take();
        self.changed = None;
        self.schedule_scan(Duration::ZERO);
    }

    /// Files changed on disk: SQL files among them are analysed again.
    pub fn files_changed(&mut self, paths: &[PathBuf]) {
        if self.root.is_none() {
            return;
        }
        let sql: Vec<PathBuf> = paths.iter().filter(|p| rc_sql::is_sql_path(p)).cloned().collect();
        if sql.is_empty() {
            return;
        }
        match (&mut self.changed, self.scan_due) {
            // A full scan is already waiting.
            (None, Some(_)) => {}
            (Some(list), _) => {
                for p in sql {
                    if !list.contains(&p) {
                        list.push(p);
                    }
                }
            }
            (None, None) => self.changed = Some(sql),
        }
        self.schedule_scan(SCAN_DELAY);
    }

    fn schedule_scan(&mut self, delay: Duration) {
        self.scan_due = Some(Instant::now() + delay);
        self.wake_after(delay);
    }

    fn wake_after(&self, delay: Duration) {
        let out = self.inbox.sender();
        Runtime::spawn(async move {
            tokio::time::sleep(delay).await;
            out.send(Event::Tick);
        });
    }

    /// Build results replaced the Problems window's entries: show ours again.
    pub fn republish(&mut self) {
        self.published.clear();
        self.problems_dirty = true;
    }

    /// Once per frame: scan results, analysis of edited files, problems.
    pub fn update(&mut self, docs: &mut Docs, problems: &mut BTreeMap<PathBuf, Vec<rc_editor::Diagnostic>>) {
        for ev in self.inbox.drain() {
            if let Event::Scanned { generation, full, files } = ev
                && generation == self.scan_gen
            {
                self.scanning = false;
                if full {
                    self.disk.clear();
                }
                for (path, analysis) in files {
                    match analysis {
                        Some(a) => self.disk.insert(path, a),
                        None => self.disk.remove(&path),
                    };
                }
                self.index.take();
            }
        }
        if let Some(due) = self.scan_due
            && Instant::now() >= due
            && !self.scanning
            && let Some(root) = self.root.clone()
        {
            self.scan_due = None;
            self.scanning = true;
            self.scan_gen += 1;
            let (out, generation) = (self.inbox.sender(), self.scan_gen);
            let changed = self.changed.take();
            std::thread::spawn(move || {
                let full = changed.is_none();
                let paths = changed.unwrap_or_else(|| sql_files(&root));
                out.send(Event::Scanned { generation, full, files: analyze_all(paths) });
            });
        }
        let open: Vec<u64> = docs.list.iter().filter(|d| is_sql(d)).map(|d| d.editor.id).collect();
        let before = self.states.len();
        self.states.retain(|id, _| open.contains(id));
        if self.states.len() != before {
            self.problems_dirty = true;
            self.index.take();
        }
        for d in docs.list.iter_mut().filter(|d| is_sql(d)) {
            self.refresh(d);
        }
        if self.problems_dirty {
            self.problems_dirty = false;
            self.sync_problems(docs, problems);
        }
    }

    fn refresh(&mut self, d: &mut Doc) {
        let id = d.editor.id;
        let version = d.editor.buffer.version();
        let now = Instant::now();
        let st = self.states.entry(id).or_insert_with(|| DocState {
            path: d.path.clone(),
            version: None,
            seen: (version, now.checked_sub(ANALYZE_DELAY).unwrap_or(now)),
            analysis: Arc::default(),
            diags: Vec::new(),
        });
        st.path = d.path.clone();
        if st.seen.0 != version {
            st.seen = (version, now);
        }
        if st.version == Some(version) {
            return;
        }
        if now - st.seen.1 < ANALYZE_DELAY {
            let wait = ANALYZE_DELAY - (now - st.seen.1);
            self.wake_after(wait);
            return;
        }
        st.version = Some(version);
        let text = d.editor.buffer.rope().to_string();
        let analysis = Arc::new(rc_sql::analyze(&text));
        let diags: Vec<rc_editor::Diagnostic> = analysis
            .problems
            .iter()
            .map(|p| rc_editor::Diagnostic {
                start: char_of(&text, p.span.start),
                end: char_of(&text, p.span.end),
                severity: if p.severity == rc_sql::Severity::Error { Severity::Error } else { Severity::Warning },
                message: p.message.clone(),
                code: Some(p.code.to_string()),
                unnecessary: false,
                deprecated: false,
            })
            .collect();
        st.analysis = analysis;
        self.index.take();
        if st.diags != diags {
            st.diags = diags;
            self.problems_dirty = true;
        }
    }

    /// Puts the open files' problems in the Problems window and their editors.
    fn sync_problems(&mut self, docs: &mut Docs, problems: &mut BTreeMap<PathBuf, Vec<rc_editor::Diagnostic>>) {
        let mut want: BTreeMap<PathBuf, Vec<rc_editor::Diagnostic>> = BTreeMap::new();
        for st in self.states.values().filter(|s| s.version.is_some()) {
            want.insert(st.path.clone(), st.diags.clone());
        }
        for p in self.published.keys() {
            want.entry(p.clone()).or_default();
        }
        for (path, list) in want {
            let old = self.published.remove(&path).unwrap_or_default();
            let entry = problems.entry(path.clone()).or_default();
            entry.retain(|d| !old.contains(d));
            for d in &list {
                if !entry.contains(d) {
                    entry.push(d.clone());
                }
            }
            if entry.is_empty() {
                problems.remove(&path);
            }
            if !list.is_empty() {
                self.published.insert(path, list);
            }
        }
        for d in docs.list.iter_mut() {
            let Some(st) = self.states.get(&d.editor.id).filter(|s| s.version.is_some()) else { continue };
            let mut all = st.diags.clone();
            all.extend(problems.get(&d.path).into_iter().flatten().filter(|x| !st.diags.contains(x)).cloned());
            d.editor.set_diagnostics(all);
        }
    }

    /// The workspace's SQL files linked together, open files as edited.
    pub fn index(&self) -> Arc<Index> {
        if let Some(index) = self.index.borrow().as_ref() {
            return index.clone();
        }
        let mut files: HashMap<PathBuf, Arc<FileAnalysis>> = self.disk.clone();
        for st in self.states.values().filter(|s| s.version.is_some()) {
            files.insert(st.path.clone(), st.analysis.clone());
        }
        let index = Arc::new(Index::new(files.into_iter().collect()));
        *self.index.borrow_mut() = Some(index.clone());
        index
    }

    /// The analysis of an open document (analysed now when typing has not paused yet).
    pub fn analysis(&self, d: &Doc) -> Arc<FileAnalysis> {
        match self.states.get(&d.editor.id) {
            Some(st) if st.version == Some(d.editor.buffer.version()) => st.analysis.clone(),
            _ => Arc::new(rc_sql::analyze(&d.editor.buffer.rope().to_string())),
        }
    }

    /// The index with `d` analysed as it is now.
    fn current_index(&mut self, d: &Doc) -> Arc<Index> {
        let fresh = self.states.get(&d.editor.id).is_some_and(|st| st.version == Some(d.editor.buffer.version()));
        if !fresh {
            let analysis = Arc::new(rc_sql::analyze(&d.editor.buffer.rope().to_string()));
            let st = self.states.entry(d.editor.id).or_insert_with(|| DocState {
                path: d.path.clone(),
                version: None,
                seen: (d.editor.buffer.version(), Instant::now()),
                analysis: Arc::default(),
                diags: Vec::new(),
            });
            st.analysis = analysis;
            // Problems follow once typing pauses; only the analysis is taken now.
            st.version = Some(d.editor.buffer.version());
            self.problems_dirty = true;
            self.index.take();
        }
        self.index()
    }

    // ------------------------------------------------------------------ editor events

    /// Reacts to what a SQL editor did this frame.
    pub fn on_editor_events(&mut self, d: &mut Doc, docs_text: &dyn Fn(&Path) -> Option<String>, events: &[EditorEvent]) -> Vec<SqlOutput> {
        let mut out = Vec::new();
        for ev in events {
            match ev {
                EditorEvent::HoverAt { pos, screen } => self.hover(d, *pos, *screen),
                EditorEvent::HoverAction(i) => {
                    if let Some((path, offset)) = self.hover_targets.get(*i).cloned() {
                        out.extend(open_output(&path, offset, docs_text));
                    }
                }
                EditorEvent::NavigateAt(pos) => out.extend(self.navigate(d, *pos, false, docs_text)),
                _ => {}
            }
        }
        out
    }

    /// F1: the hover of the caret's name, under the caret.
    pub fn quick_doc(&mut self, d: &mut Doc, screen: egui::Pos2) {
        self.hover(d, d.editor.caret(), screen);
    }

    fn hover(&mut self, d: &mut Doc, pos: usize, screen: egui::Pos2) {
        let problem = d.editor.diagnostic_at(pos).map(|x| (x.severity, x.message.clone()));
        let index = self.current_index(d);
        let rope = d.editor.buffer.rope();
        let byte = rope.char_to_byte(pos.min(rope.len_chars()));
        let keys = match index.at(&d.path, byte) {
            Some(Hit::Definition(key)) => vec![key],
            Some(Hit::Reference(_, keys)) => keys,
            None => Vec::new(),
        };
        self.hover_targets.clear();
        let mut lines = Vec::new();
        let mut actions = Vec::new();
        for key in keys.iter().take(3) {
            let Some((path, def)) = index.primary(key) else { continue };
            if !lines.is_empty() {
                lines.push("---".into());
            }
            lines.extend(describe(&index, path, def, self.root.as_deref()));
            let label = if keys.len() > 1 { trf("Go to {name}", &[("name", &key.to_string())]) } else { tr("Go to Declaration").to_string() };
            actions.push((ico::OPEN, label));
            self.hover_targets.push((path.to_path_buf(), def.name.start));
        }
        if lines.is_empty() && problem.is_none() {
            return;
        }
        d.editor.popups.hover = Some(Hover { pos, text: lines.join("\n"), problem, screen, actions });
    }

    /// Go to Declaration (`usages` false) or Find Usages at char `pos`. On a definition's name,
    /// Go to Declaration lists its usages (Rider).
    pub fn navigate(&mut self, d: &Doc, pos: usize, usages: bool, docs_text: &dyn Fn(&Path) -> Option<String>) -> Vec<SqlOutput> {
        let index = self.current_index(d);
        let rope = d.editor.buffer.rope();
        let byte = rope.char_to_byte(pos.min(rope.len_chars()));
        let (keys, on_definition) = match index.at(&d.path, byte) {
            Some(Hit::Definition(key)) => (vec![key], true),
            Some(Hit::Reference(_, keys)) => (keys, false),
            None => (Vec::new(), false),
        };
        if keys.is_empty() {
            return vec![SqlOutput::Message(if usages { tr("No usages found") } else { tr("Cannot find declaration to go to") }.into())];
        }
        if usages || on_definition {
            let mut targets = Vec::new();
            let mut texts: HashMap<PathBuf, String> = HashMap::new();
            for key in &keys {
                for (path, r) in index.usages(key) {
                    let text = texts.entry(path.to_path_buf()).or_insert_with(|| docs_text(path).unwrap_or_default());
                    targets.push(target(path, text, r.span.start));
                }
            }
            if targets.is_empty() {
                return vec![SqlOutput::Message(tr("No usages found").into())];
            }
            let title = trf("Usages of {name}", &[("name", &keys[0].qualified())]);
            return vec![SqlOutput::Usages { title, targets }];
        }
        let places: Vec<(PathBuf, usize)> = keys.iter().filter_map(|k| index.primary(k)).map(|(p, def)| (p.to_path_buf(), def.name.start)).collect();
        match places.as_slice() {
            [] => vec![SqlOutput::Message(tr("Cannot find declaration to go to").into())],
            [(path, offset)] => open_output(path, *offset, docs_text).into_iter().collect(),
            _ => {
                let targets = places.iter().map(|(p, o)| target(p, &docs_text(p).unwrap_or_default(), *o)).collect();
                vec![SqlOutput::Usages { title: trf("Declarations of {name}", &[("name", &keys[0].qualified())]), targets }]
            }
        }
    }

    #[cfg(test)]
    pub fn idle_for_test(&self) -> bool {
        self.scan_due.is_none() && !self.scanning
    }

    #[cfg(test)]
    pub fn files_for_test(&self) -> usize {
        self.disk.len()
    }
}

fn target(path: &Path, text: &str, offset: usize) -> Target {
    let (line, col) = line_col(text, offset);
    Target { path: path.to_path_buf(), line, col, preview: text.lines().nth(line).unwrap_or_default().trim().to_string() }
}

fn open_output(path: &Path, offset: usize, docs_text: &dyn Fn(&Path) -> Option<String>) -> Option<SqlOutput> {
    let text = docs_text(path)?;
    let (line, col) = line_col(&text, offset);
    Some(SqlOutput::Open { path: path.to_path_buf(), line, col })
}

/// The hover lines of an object: signature, kind and place, changeset comment, columns.
fn describe(index: &Index, path: &Path, def: &Definition, root: Option<&Path>) -> Vec<String> {
    let mut lines = vec![code(&def.signature())];
    let file = root.and_then(|r| path.strip_prefix(r).ok()).unwrap_or(path).display().to_string();
    let mut facts = vec![kind_title(def.kind).to_string()];
    if let Some(t) = &def.table {
        facts.push(trf("on {table}", &[("table", &code(&t.to_string()))]));
    }
    if let Some(l) = &def.language {
        facts.push(l.clone());
    }
    facts.push(code(&file));
    lines.push(facts.join(" · "));
    if let Some(c) = &def.changeset {
        lines.push(trf("Changeset {id}", &[("id", &code(c))]));
    }
    if !def.comment.is_empty() {
        lines.push(def.comment.clone());
    }
    let columns = if matches!(def.kind, ObjKind::Table | ObjKind::View | ObjKind::MaterializedView | ObjKind::Type) { index.columns(&def.key) } else { Vec::new() };
    if !columns.is_empty() {
        lines.push("---".into());
        const SHOWN: usize = 16;
        for c in columns.iter().take(SHOWN) {
            let mut s = code(&c.name);
            if !c.ty.is_empty() {
                s.push(' ');
                s.push_str(&c.ty);
            }
            if c.not_null {
                s.push_str(" NOT NULL");
            }
            lines.push(s);
        }
        if columns.len() > SHOWN {
            lines.push(trf("…and {n} more", &[("n", &(columns.len() - SHOWN))]));
        }
    }
    lines
}

/// The kind of an object, translated.
pub fn kind_title(kind: ObjKind) -> &'static str {
    tr(match kind {
        ObjKind::Schema => "Schema",
        ObjKind::Table => "Table",
        ObjKind::View => "View",
        ObjKind::MaterializedView => "Materialized view",
        ObjKind::Function => "Function",
        ObjKind::Procedure => "Procedure",
        ObjKind::Type => "Type",
        ObjKind::Domain => "Domain",
        ObjKind::Sequence => "Sequence",
        ObjKind::Index => "Index",
        ObjKind::Trigger => "Trigger",
        ObjKind::Extension => "Extension",
    })
}

/// The declarations of an open document, for the Structure window and Go to Symbol.
pub fn structure(analysis: &FileAnalysis) -> Vec<&Definition> {
    analysis.definitions.iter().filter(|d| !d.is_alter).collect()
}

/// The Structure / Go to Symbol icon of a kind.
pub fn symbol_kind(kind: ObjKind) -> rc_lsp::lsp_types::SymbolKind {
    use rc_lsp::lsp_types::SymbolKind as K;
    match kind {
        ObjKind::Schema => K::NAMESPACE,
        ObjKind::Table => K::CLASS,
        ObjKind::View | ObjKind::MaterializedView => K::INTERFACE,
        ObjKind::Function => K::FUNCTION,
        ObjKind::Procedure => K::METHOD,
        ObjKind::Type => K::STRUCT,
        ObjKind::Domain => K::TYPE_PARAMETER,
        ObjKind::Sequence => K::CONSTANT,
        ObjKind::Index => K::KEY,
        ObjKind::Trigger => K::EVENT,
        ObjKind::Extension => K::PACKAGE,
    }
}
