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

/// A rendered diagram: its image (`bytes://…svg`) and its size in points.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagram {
    pub uri: String,
    pub size: egui::Vec2,
}

/// Diagrams already rendered: (source hash, dark) → the diagram or the error.
type MermaidMap = std::collections::HashMap<(u64, bool), Result<Diagram, String>>;

#[derive(Clone, Default)]
struct MermaidCache(std::sync::Arc<std::sync::Mutex<MermaidMap>>);

/// Diagrams kept before the cache starts over (typing in a block makes a new one per edit).
const MERMAID_CACHE: usize = 64;

/// The diagram of `source` as an image URI the preview can show, or why it can't be drawn.
fn mermaid_image(ctx: &egui::Context, source: &str, dark: bool) -> Result<Diagram, String> {
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
        for d in map.drain().filter_map(|(_, r)| r.ok()) {
            ctx.forget_image(&d.uri);
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
            let size = svg_size(&svg).unwrap_or(vec2(800.0, 400.0));
            ctx.include_bytes(uri.clone(), svg.into_bytes());
            Ok(Diagram { uri, size })
        }
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(tr("The diagram could not be drawn").to_string()),
    };
    map.insert(key, result.clone());
    result
}

/// The size of an SVG from its `viewBox` (else its `width` and `height`).
fn svg_size(svg: &str) -> Option<egui::Vec2> {
    let tag = &svg[svg.find("<svg")?..];
    let tag = &tag[..tag.find('>')?];
    let attr = |name: &str| -> Option<&str> {
        let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
        Some(&tag[at..at + tag[at..].find('"')?])
    };
    if let Some(vb) = attr("viewBox") {
        let n: Vec<f32> = vb.split([' ', ',']).filter_map(|x| x.trim().parse().ok()).collect();
        if let [_, _, w, h] = n[..]
            && w > 0.0
            && h > 0.0
        {
            return Some(vec2(w, h));
        }
    }
    let num = |s: &str| s.trim_end_matches("px").parse::<f32>().ok().filter(|v| *v > 0.0);
    Some(vec2(num(attr("width")?)?, num(attr("height")?)?))
}

/// A page cut at its diagrams: Markdown, then a diagram, then Markdown…
#[derive(Clone, Debug, PartialEq)]
pub enum Part {
    Markdown(String),
    Diagram(Diagram),
}

/// `text` cut at each ```mermaid (or ~~~mermaid) block that renders; a block that does not
/// render stays as code with the error under it.
pub fn split_mermaid(ctx: &egui::Context, text: &str, dark: bool) -> Vec<Part> {
    if !text.contains("mermaid") {
        return vec![Part::Markdown(text.to_string())];
    }
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut parts = Vec::new();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();
        let indent = line.len() - t.len();
        let fence: String = t.chars().take_while(|&c| c == '`' || c == '~').collect();
        let opens = indent < 4 && fence.len() >= 3 && fence.chars().all(|c| fence.starts_with(c));
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
        match mermaid_image(ctx, &source, dark) {
            Ok(diagram) => {
                if !out.trim().is_empty() {
                    parts.push(Part::Markdown(std::mem::take(&mut out)));
                }
                out.clear();
                parts.push(Part::Diagram(diagram));
            }
            Err(e) => {
                out.push_str(&lines[i..=close].concat());
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                let pad = " ".repeat(indent);
                out.push_str(&format!("\n{pad}> {}\n", trf("Mermaid: {error}", &[("error", &e.lines().next().unwrap_or_default())])));
            }
        }
        i = close + 1;
    }
    if !out.trim().is_empty() || parts.is_empty() {
        parts.push(Part::Markdown(out));
    }
    parts
}

/// The open enlarged diagram: `zoom` None fits it to the window.
#[derive(Clone, Debug, PartialEq)]
struct Zoomed {
    diagram: Diagram,
    zoom: Option<f32>,
}

fn zoom_id() -> egui::Id {
    egui::Id::new("mermaid_zoom")
}

/// A diagram of the page, at most `width` wide; a click enlarges it.
fn diagram(ui: &mut Ui, d: &Diagram, width: f32) {
    let size = if d.size.x > width { d.size * (width / d.size.x) } else { d.size };
    let resp = ui
        .add(egui::Image::new(d.uri.as_str()).fit_to_exact_size(size).sense(egui::Sense::click()))
        .on_hover_cursor(egui::CursorIcon::ZoomIn)
        .on_hover_text(tr("Click to enlarge"));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tr("Mermaid diagram")));
    if resp.clicked() {
        ui.ctx().data_mut(|data| data.insert_temp(zoom_id(), Zoomed { diagram: d.clone(), zoom: None }));
    }
}

/// The enlarged diagram over the window: Fit, 100 %, −, +, ⌘/Ctrl+wheel to zoom, drag to
/// move, Esc or a click beside it to close.
fn zoom_window(ctx: &egui::Context) {
    let Some(mut z) = ctx.data(|d| d.get_temp::<Zoomed>(zoom_id())) else { return };
    let screen = ctx.content_rect();
    let avail = (screen.size() * 0.9 - vec2(32.0, 80.0)).max(vec2(200.0, 150.0));
    let fit = (avail.x / z.diagram.size.x).min(avail.y / z.diagram.size.y).clamp(0.05, 4.0);
    let mut close = false;
    let resp = egui::Modal::new(egui::Id::new("mermaid_zoom_modal")).show(ctx, |ui| {
        let scale = z.zoom.unwrap_or(fit);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(tr("Mermaid diagram")).strong());
            ui.label(format!("{:.0} %", scale * 100.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if rc_kit::theme::icon_button(ui, rc_kit::ico::CLOSE, true, tr("Close")).clicked() {
                    close = true;
                }
                if ui.button("+").on_hover_text(tr("Zoom In")).clicked() {
                    z.zoom = Some((scale * 1.25).min(8.0));
                }
                if ui.button("−").on_hover_text(tr("Zoom Out")).clicked() {
                    z.zoom = Some((scale / 1.25).max(0.05));
                }
                if ui.button("100 %").clicked() {
                    z.zoom = Some(1.0);
                }
                if ui.button(tr("Fit")).clicked() {
                    z.zoom = None;
                }
            });
        });
        ui.separator();
        // One size for the window whatever the zoom: its buttons stay where they are.
        ui.set_width(avail.x);
        egui::ScrollArea::both()
            .id_salt("mermaid_zoom_scroll")
            .min_scrolled_width(avail.x)
            .min_scrolled_height(avail.y)
            .max_width(avail.x)
            .max_height(avail.y)
            .auto_shrink(false)
            .show(ui, |ui| {
                let size = z.diagram.size * scale;
                // Smaller than the window: centred.
                let pad = ((avail - size) / 2.0).max(egui::Vec2::ZERO);
                ui.add_space(pad.y);
                ui.horizontal(|ui| {
                    ui.add_space(pad.x);
                    let image = ui.add(egui::Image::new(z.diagram.uri.as_str()).fit_to_exact_size(size).sense(egui::Sense::drag()));
                    if image.dragged() {
                        ui.scroll_with_delta(image.drag_delta());
                    }
                });
            });
        // ⌘/Ctrl+wheel and pinch zoom.
        let factor = ui.input(|i| i.zoom_delta());
        if factor != 1.0 {
            z.zoom = Some((scale * factor).clamp(0.05, 8.0));
        }
    });
    if close || resp.should_close() {
        ctx.data_mut(|d| d.remove::<Zoomed>(zoom_id()));
    } else {
        ctx.data_mut(|d| d.insert_temp(zoom_id(), z));
    }
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
    let parts = split_mermaid(ui.ctx(), &doc.editor.buffer.rope().to_string(), p.dark);
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
            for (k, part) in parts.iter().enumerate() {
                match part {
                    Part::Markdown(text) => {
                        ui.push_id(k, |ui| {
                            egui_commonmark::CommonMarkViewer::new().default_implicit_uri_scheme(base.clone()).max_image_width(Some(width.max(100.0) as usize)).show(ui, md_cache, text);
                        });
                    }
                    Part::Diagram(d) => diagram(ui, d, width.max(100.0)),
                }
            }
        });
    });
    // Text selected in the page was copied while it was drawn: the editor beside it must not
    // copy its own line over it.
    if ui.ctx().with_plugin::<egui::text_selection::LabelSelectionState, _>(|s| s.has_selection()).unwrap_or(false) {
        ui.ctx().input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Copy | egui::Event::Cut)));
    }
    zoom_window(ui.ctx());
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
        let with_mermaid = |ctx: &egui::Context, text: &str, dark: bool| -> String {
            split_mermaid(ctx, text, dark)
                .into_iter()
                .map(|p| match p {
                    Part::Markdown(m) => m,
                    Part::Diagram(d) => format!("![Mermaid diagram]({})\n", d.uri),
                })
                .collect()
        };
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
    fn a_click_on_a_diagram_enlarges_it() {
        use egui_kittest::kittest::Queryable;
        let _lock = crate::screens::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("modules.md");
        std::fs::write(&path, "# Modules\n\n```mermaid\nflowchart LR\n  a[\"00 platform\"] --> b[\"10 tenancy\"] --> c[\"20 clients\"]\n```\n\n- after the diagram\n").unwrap();
        let mut app = crate::screens::test_app(home.path(), true);
        app.open_file(&path);
        app.docs.active_doc_mut().unwrap().md_view = Some(MdView::Preview);
        let mut h = crate::screens::harness(app, egui::vec2(1280.0, 800.0));
        h.run_steps(3);
        assert!(h.query_by_label("after the diagram").is_some(), "the page goes on after the diagram");
        let zoomed = |h: &egui_kittest::Harness<'static, crate::app::DotCraftApp>| h.ctx.data(|d| d.get_temp::<Zoomed>(zoom_id()));
        assert!(zoomed(&h).is_none());
        h.get_by_label(tr("Mermaid diagram")).click();
        h.run_steps(3);
        let z = zoomed(&h).expect("the enlarged diagram");
        assert_eq!(z.zoom, None, "fits the window first");
        h.get_by_label("+").click();
        h.run_steps(2);
        let zoom = zoomed(&h).unwrap().zoom.expect("zoomed in");
        h.get_by_label("+").click();
        h.run_steps(2);
        assert!(zoomed(&h).unwrap().zoom.unwrap() > zoom);
        h.get_by_label("100 %").click();
        h.run_steps(2);
        assert_eq!(zoomed(&h).unwrap().zoom, Some(1.0));
        h.get_by_label(tr("Fit")).click();
        h.run_steps(2);
        assert_eq!(zoomed(&h).unwrap().zoom, None);
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        assert!(zoomed(&h).is_none(), "Esc closes it");
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
        use egui_kittest::kittest::Queryable;
        h.get_by_label(tr("Mermaid diagram")).click();
        for _ in 0..10 {
            h.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        h.render().unwrap().save(out.join("markdown-mermaid-zoom-dark.png")).unwrap();
    }
}
