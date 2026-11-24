//! Markdown preview (Rider, VS Code): a `.md` file shows its editor, the rendered page or both
//! side by side, switched from the buttons at the top right; the preview follows the editor's
//! scroll and updates as you type. ```mermaid blocks show their diagram.

use crate::app::DotCraftApp;
use crate::docs::MdView;
use egui::{Rect, Ui, vec2};
use rc_kit::i18n::{tr, trf};
use rc_kit::{ico, theme};

/// The editor's and the preview's parts of `rect` for `view`.
pub fn layout(view: MdView, rect: Rect) -> (Option<Rect>, Option<Rect>) {
    match view {
        MdView::Editor => (Some(rect), None),
        MdView::Preview => (None, Some(rect)),
        MdView::Split => {
            let mid = rect.center().x.round();
            (Some(Rect::from_min_max(rect.min, egui::pos2(mid, rect.max.y))), Some(Rect::from_min_max(egui::pos2(mid + 1.0, rect.min.y), rect.max)))
        }
    }
}

/// Diagrams already rendered: (source hash, dark) → the image's `bytes://` URI or the error.
type MermaidMap = std::collections::HashMap<(u64, bool), Result<String, String>>;

#[derive(Clone, Default)]
struct MermaidCache(std::sync::Arc<std::sync::Mutex<MermaidMap>>);

/// Diagrams kept before the cache starts over (typing in a block makes a new one per edit).
const MERMAID_CACHE: usize = 64;

/// The diagram of `source` as an image URI the preview can show, or why it can't be drawn.
fn mermaid_image(ctx: &egui::Context, source: &str, dark: bool) -> Result<String, String> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    let key = (h.finish(), dark);
    let cache = ctx.data_mut(|d| d.get_temp_mut_or_default::<MermaidCache>(egui::Id::new("mermaid_cache")).clone());
    let mut map = cache.0.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(r) = map.get(&key) {
        return r.clone();
    }
    if map.len() >= MERMAID_CACHE {
        for uri in map.drain().filter_map(|(_, r)| r.ok()) {
            ctx.forget_image(&uri);
        }
    }
    let options = mermaid_rs_renderer::RenderOptions {
        theme: if dark { mermaid_rs_renderer::Theme::dark() } else { mermaid_rs_renderer::Theme::modern() },
        ..Default::default()
    };
    // A renderer bug must not take the IDE down with it.
    let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mermaid_rs_renderer::render_with_options(source, options)));
    let result = match rendered {
        Ok(Ok(svg)) => {
            let uri = format!("bytes://mermaid-{:016x}-{}.svg", key.0, if dark { "dark" } else { "light" });
            ctx.include_bytes(uri.clone(), svg.into_bytes());
            Ok(uri)
        }
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(tr("The diagram could not be drawn").to_string()),
    };
    map.insert(key, result.clone());
    result
}

/// `text` with each ```mermaid (or ~~~mermaid) block replaced by an image of its diagram; a
/// block that does not render stays as code with the error under it.
pub fn with_mermaid(ctx: &egui::Context, text: &str, dark: bool) -> String {
    if !text.contains("mermaid") {
        return text.to_string();
    }
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();
        let indent = line.len() - t.len();
        let fence: String = t.chars().take_while(|&c| c == '`' || c == '~').collect();
        let opens = indent < 4 && fence.len() >= 3 && fence.chars().all(|c| c == fence.chars().next().unwrap_or('`'));
        let info = t[fence.len()..].trim();
        let is_mermaid = opens && info.split_whitespace().next().is_some_and(|w| w.eq_ignore_ascii_case("mermaid"));
        let close = if is_mermaid {
            (i + 1..lines.len()).find(|&j| {
                let c = lines[j].trim();
                c.len() >= fence.len() && c.chars().all(|ch| fence.starts_with(ch))
            })
        } else {
            None
        };
        let Some(close) = close else {
            out.push_str(line);
            i += 1;
            continue;
        };
        let source: String = lines[i + 1..close].concat();
        let pad = " ".repeat(indent);
        match mermaid_image(ctx, &source, dark) {
            Ok(uri) => out.push_str(&format!("{pad}![{}]({uri})\n", tr("Mermaid diagram"))),
            Err(e) => {
                out.push_str(&lines[i..=close].concat());
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(&format!("\n{pad}> {}\n", trf("Mermaid: {error}", &[("error", &e.lines().next().unwrap_or_default())])));
            }
        }
        i = close + 1;
    }
    out
}

/// The rendered page of document `idx` in `rect`. Returns whether it was clicked.
pub fn preview(app: &mut DotCraftApp, ui: &mut Ui, idx: usize, rect: Rect) -> bool {
    let p = rc_kit::pal(ui);
    let DotCraftApp { docs, md_cache, .. } = app;
    let doc = &docs.list[idx];
    let id = doc.editor.id;
    ui.painter().rect_filled(rect, 0.0, p.editor_bg);
    if doc.md_view == Some(MdView::Split) {
        ui.painter().vline(rect.left() - 0.5, rect.y_range(), egui::Stroke::new(1.0, p.island_border));
    }
    let text = with_mermaid(ui.ctx(), &doc.editor.buffer.rope().to_string(), p.dark);
    // Relative images (`![](docs/shot.png)`) load from the file's folder.
    let base = doc.path.parent().map(|d| format!("file://{}/", d.display())).unwrap_or_default();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("md_preview", id)));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    // Page sizes rather than the IDE's compact ones: headings scale between Body and Heading.
    {
        use egui::{FontId, TextStyle};
        let styles = &mut child.style_mut().text_styles;
        styles.insert(TextStyle::Heading, theme::bold(26.0));
        styles.insert(TextStyle::Body, FontId::proportional(14.0));
        styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
        child.style_mut().spacing.item_spacing.y = 6.0;
        // The page's text can be selected and copied (the IDE's labels can't).
        child.style_mut().interaction.selectable_labels = true;
    }
    let mut area = egui::ScrollArea::vertical().id_salt(("md_scroll", id)).auto_shrink(false);
    // Side by side, the page follows the editor when the editor scrolls.
    let frac = doc.editor.scroll_fraction();
    let key = egui::Id::new(("md_sync", id));
    let (last, max): (Option<f32>, f32) = ui.data(|d| d.get_temp(key)).unwrap_or((None, 0.0));
    if doc.md_view == Some(MdView::Split) && last.is_some_and(|l| (l - frac).abs() > f32::EPSILON) {
        area = area.vertical_scroll_offset(frac * max);
    }
    let width = rect.width() - 56.0;
    let out = area.show(&mut child, |ui| {
        egui::Frame::new().inner_margin(egui::Margin::symmetric(28, 20)).show(ui, |ui| {
            ui.set_width(width.max(100.0));
            egui_commonmark::CommonMarkViewer::new().default_implicit_uri_scheme(base).max_image_width(Some(width.max(100.0) as usize)).show(ui, md_cache, &text);
        });
    });
    // Text selected in the page was copied while it was drawn: the editor beside it must not
    // copy its own line over it.
    if ui.ctx().with_plugin::<egui::text_selection::LabelSelectionState, _>(|s| s.has_selection()).unwrap_or(false) {
        ui.ctx().input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Copy | egui::Event::Cut)));
    }
    let max = (out.content_size.y - out.inner_rect.height()).max(0.0);
    ui.data_mut(|d| d.insert_temp(key, (Some(frac), max)));
    ui.input(|i| i.pointer.primary_pressed() && i.pointer.interact_pos().is_some_and(|p| rect.contains(p)))
}

/// Editor / Editor and Preview / Preview, floating at the top right of the pane.
pub fn switcher(app: &mut DotCraftApp, ui: &mut Ui, idx: usize, rect: Rect) {
    let Some(current) = app.docs.list[idx].md_view else { return };
    let p = rc_kit::pal(ui);
    let id = app.docs.list[idx].editor.id;
    let size = vec2(3.0 * 26.0 + 2.0 * 2.0 + 8.0, 34.0);
    let at = egui::pos2(rect.right() - size.x - 22.0, rect.top() + 6.0);
    egui::Area::new(egui::Id::new(("md_switch", id, rect.left() as i32))).order(egui::Order::Foreground).fixed_pos(at).show(ui.ctx(), |ui| {
        egui::Frame::new().fill(p.island_bg).stroke(egui::Stroke::new(1.0, p.popup_border)).corner_radius(8.0).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (view, icon, tip) in [(MdView::Editor, ico::EDIT, tr("Editor Only")), (MdView::Split, ico::SPLIT_RIGHT, tr("Editor and Preview")), (MdView::Preview, ico::PREVIEW, tr("Preview Only"))] {
                    let resp = theme::icon_button(ui, icon, true, tip);
                    if view == current {
                        ui.painter().rect_stroke(resp.rect, 6.0, egui::Stroke::new(1.0, p.accent), egui::StrokeKind::Inside);
                    }
                    if resp.clicked() {
                        app.docs.list[idx].md_view = Some(view);
                        app.focus_editor = view != MdView::Preview;
                    }
                }
            });
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_files_open_beside_their_preview() {
        let dir = tempfile::tempdir().unwrap();
        let (md, cs) = (dir.path().join("README.md"), dir.path().join("Program.cs"));
        std::fs::write(&md, "# Hi\n").unwrap();
        std::fs::write(&cs, "class A {}\n").unwrap();
        let mut docs = crate::docs::Docs::new(rc_syntax::ParserPool::new(std::sync::Arc::new(|| {})));
        let i = docs.open(&md).unwrap();
        assert_eq!(docs.list[i].md_view, Some(MdView::Split));
        let j = docs.open(&cs).unwrap();
        assert_eq!(docs.list[j].md_view, None);
        let r = Rect::from_min_size(egui::pos2(0.0, 0.0), vec2(800.0, 600.0));
        let (ed, preview) = layout(MdView::Split, r);
        assert_eq!((ed.unwrap().width(), preview.unwrap().left()), (400.0, 401.0));
        assert_eq!(layout(MdView::Preview, r), (None, Some(r)));
    }

    /// Drags over `label` in the preview, sends Copy and returns what reached the clipboard.
    fn copy_from_preview(view: MdView) -> Vec<String> {
        use egui_kittest::kittest::Queryable;
        let _lock = crate::screens::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("notes.md");
        std::fs::write(&path, "# Notes\n\nHello preview world\n").unwrap();
        let mut app = crate::screens::test_app(home.path(), true);
        app.open_file(&path);
        app.docs.active_doc_mut().unwrap().md_view = Some(view);
        let mut h = crate::screens::harness(app, egui::vec2(1280.0, 800.0));
        h.run_steps(3);
        let rect = h.get_by_label("Hello preview world").rect();
        let (from, to) = (egui::pos2(rect.left() + 1.0, rect.center().y), egui::pos2(rect.right() - 1.0, rect.center().y));
        h.event(egui::Event::PointerMoved(from));
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        h.run_steps(1);
        for k in 1..=10 {
            h.event(egui::Event::PointerMoved(from + (to - from) * (k as f32 / 10.0)));
            h.run_steps(1);
        }
        h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        h.run_steps(2);
        h.event(egui::Event::Copy);
        let mut copied = Vec::new();
        for _ in 0..4 {
            h.run_steps(1);
            copied.extend(h.output().platform_output.commands.iter().filter_map(|c| if let egui::OutputCommand::CopyText(t) = c { Some(t.clone()) } else { None }));
        }
        copied
    }

    /// Selects "hello" in the editor of `file`, right-clicks it and picks Copy.
    fn copy_from_editor_menu(file: &str, view: Option<MdView>) -> Vec<String> {
        use egui_kittest::kittest::Queryable;
        let _lock = crate::screens::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(file);
        std::fs::write(&path, "hello world\n").unwrap();
        let mut app = crate::screens::test_app(home.path(), true);
        app.open_file(&path);
        let doc = app.docs.active_doc_mut().unwrap();
        doc.md_view = view;
        doc.editor.selection = rc_text::Selection::single(rc_text::Range::new(0, 5));
        let mut h = crate::screens::harness(app, egui::vec2(1280.0, 800.0));
        h.run_steps(3);
        // Right-click on the selected word (the caret is at its end).
        let caret = h.state().caret_screen.expect("caret on screen");
        let at = caret + egui::vec2(-12.0, -8.0);
        h.event(egui::Event::PointerMoved(at));
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed: true, modifiers: Default::default() });
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed: false, modifiers: Default::default() });
        h.run_steps(3);
        h.get_by_label(tr("Copy")).click();
        let mut copied = Vec::new();
        for _ in 0..4 {
            h.run_steps(1);
            copied.extend(h.output().platform_output.commands.iter().filter_map(|c| if let egui::OutputCommand::CopyText(t) = c { Some(t.clone()) } else { None }));
        }
        // The copy is confirmed.
        assert!(h.state().notices.iter().any(|n| n.text == "Copied to clipboard (5 characters)"), "{:?}", h.state().notices.iter().map(|n| &n.text).collect::<Vec<_>>());
        copied
    }

    #[test]
    fn copy_in_the_editor_menu_copies_the_selection() {
        assert_eq!(copy_from_editor_menu("notes.txt", None), ["hello"], "plain file");
        assert_eq!(copy_from_editor_menu("notes.md", Some(MdView::Split)), ["hello"], "markdown beside its preview");
    }

    #[test]
    fn mermaid_blocks_become_images_and_errors_stay_as_code() {
        let ctx = egui::Context::default();
        let text = "# Modules\n\n```mermaid\nflowchart LR\n  a[\"00 platform\"] --> b[\"10 tenancy\"]\n```\n\nAfter.\n\n~~~ mermaid\nnot a diagram at all\n~~~\n\n```sql\nselect 1;\n```\n";
        let page = with_mermaid(&ctx, text, true);
        let uri = page.lines().find_map(|l| l.strip_prefix("![Mermaid diagram](").and_then(|r| r.strip_suffix(')'))).expect("an image").to_string();
        assert!(uri.starts_with("bytes://mermaid-") && uri.ends_with("-dark.svg"), "{page}");
        let Ok(egui::load::BytesPoll::Ready { bytes, .. }) = ctx.try_load_bytes(&uri) else { panic!("the SVG is registered") };
        assert!(String::from_utf8_lossy(&bytes).contains("<svg") && String::from_utf8_lossy(&bytes).contains("tenancy"));
        assert!(page.contains("After.") && page.contains("```sql\nselect 1;\n```"), "{page}");
        assert!(page.contains("~~~ mermaid\nnot a diagram at all\n~~~") && page.contains("> Mermaid: "), "{page}");
        // The same source is rendered once.
        assert_eq!(with_mermaid(&ctx, text, true), page);
        assert_eq!(with_mermaid(&ctx, "no diagrams", false), "no diagrams");
    }

    #[test]
    fn text_selected_in_the_preview_is_copied() {
        for view in [MdView::Preview, MdView::Split] {
            let copied = copy_from_preview(view);
            assert_eq!(copied, ["Hello preview world"], "{view:?}");
        }
    }
}

#[cfg(test)]
mod screens {
    use super::*;

    #[test]
    #[ignore = "Requires a GPU; writes target/snapshots/markdown-mermaid-dark.png"]
    fn screens_markdown_mermaid() {
        let _lock = crate::screens::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
        std::fs::create_dir_all(&out).unwrap();
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("ARCHITECTURE.md");
        std::fs::write(&path, "## Bounded contexts (modules)\n\nOne database, one schema set per module.\n\n```mermaid\nflowchart LR\n  platform[\"00 platform<br/>users, actor\"]\n  t[\"10 tenancy<br/>offices, memberships\"]\n  c[\"20 clients<br/>companies, access\"]\n  d[\"30 documents<br/>periods, files\"]\n  platform --> t --> c --> d\n```\n\n| nn | Module |\n|---|---|\n| 00 | `platform` |\n| 10 | `tenancy` |\n").unwrap();
        let mut app = crate::screens::test_app(home.path(), true);
        app.open_file(&path);
        let mut h = crate::screens::harness(app, vec2(1280.0, 800.0));
        for _ in 0..20 {
            h.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        h.render().unwrap().save(out.join("markdown-mermaid-dark.png")).unwrap();
    }
}
