//! PostgreSQL files through the whole app (offscreen harness): the workspace's SQL files are
//! indexed without a database, then hover, ⌘+click, Find Usages, problems while typing and
//! files changed on disk.

use crate::app::DotCraftApp;
use crate::package_insight_tests::{caret_at, text, type_text};
use crate::screens::{HOME_LOCK, harness, test_app};
use egui::vec2;
use egui_kittest::Harness;
use rc_editor::EditorEvent;
use std::path::{Path, PathBuf};

const CRM: &str = "--liquibase formatted sql

--changeset ada:20260101-01
--comment: Customers of the shop
create table crm.customers (
    id uuid primary key,
    name text not null
);

--changeset ada:20260101-02
alter table crm.customers add column email text;
";

const ORDERS: &str = "--liquibase formatted sql

--changeset bob:20260102-01
create table orders.orders (
    id uuid primary key,
    customer_id uuid not null references crm.customers (id)
);

--changeset bob:20260102-02
create or replace function orders_app.place(p_customer uuid) returns uuid language plpgsql as $$
begin
    perform 1 from crm.customers where id = p_customer;
    insert into orders.orders (id, customer_id) values (gen_random_uuid(), p_customer);
    return p_customer;
end
$$;
";

fn workspace() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::Builder::new().prefix("dotcraft-sql").tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("shop");
    std::fs::create_dir_all(root.join("db/changelog")).unwrap();
    std::fs::write(root.join("db/changelog/01-crm.sql"), CRM).unwrap();
    std::fs::write(root.join("db/changelog/02-orders.sql"), ORDERS).unwrap();
    std::fs::write(root.join("README.md"), "# Shop\n").unwrap();
    (tmp, root)
}

/// Runs frames until `done` and the SQL scan is over, at most 20 s.
fn settle(h: &mut Harness<'static, DotCraftApp>, what: &str, mut done: impl FnMut(&DotCraftApp) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        h.run_steps(1);
        if h.state().sql.idle_for_test() && done(h.state()) {
            h.run_steps(2);
            return;
        }
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn open(root: &Path, home: &Path) -> Harness<'static, DotCraftApp> {
    let mut app = test_app(home, true);
    app.settings.roslyn_path = Some("/nonexistent".into());
    app.open_workspace(root);
    app.open_file(&root.join("db/changelog/02-orders.sql"));
    let mut h = harness(app, vec2(1280.0, 800.0));
    settle(&mut h, "the SQL index", |a| a.sql.files_for_test() == 2);
    h
}

/// Char offset of the `nth` `needle` in the active editor, plus `skip` chars.
fn pos_of(app: &DotCraftApp, needle: &str, nth: usize, skip: usize) -> usize {
    let t = text(app);
    let b = t.match_indices(needle).nth(nth).unwrap().0;
    t[..b].chars().count() + skip
}

fn editor_events(h: &mut Harness<'static, DotCraftApp>, events: &[EditorEvent]) {
    let outputs = {
        let app = h.state_mut();
        let docs_text = app.docs_text();
        let i = app.docs.active.unwrap();
        app.sql.on_editor_events(&mut app.docs.list[i], &docs_text, events)
    };
    h.state_mut().handle_sql(outputs);
    h.run_steps(2);
}

#[test]
fn hover_and_navigation_across_the_workspace() {
    let _lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let (_tmp, root) = workspace();
    let mut h = open(&root, home.path());
    assert_eq!(h.state().docs.active_doc().unwrap().editor.lang(), rc_syntax::Lang::Sql);
    assert!(h.state().docs.active_doc().unwrap().editor.diagnostics.is_empty());

    // Hover on the foreign key's table: signature, file, changeset, columns after ALTER TABLE.
    let pos = pos_of(h.state(), "crm.customers", 0, 5);
    editor_events(&mut h, &[EditorEvent::HoverAt { pos, screen: egui::pos2(300.0, 300.0) }]);
    let hover = h.state().docs.active_doc().unwrap().editor.popups.hover.clone().expect("hover");
    for part in ["`crm.customers`", "`db/changelog/01-crm.sql`", "`ada:20260101-01`", "Customers of the shop", "`email` text"] {
        assert!(hover.text.contains(part), "{part} in {}", hover.text);
    }
    assert_eq!(hover.actions.len(), 1);

    // ⌘+click in the PL/pgSQL body opens the table's CREATE in the other file.
    let pos = pos_of(h.state(), "crm.customers", 1, 4);
    editor_events(&mut h, &[EditorEvent::NavigateAt(pos)]);
    let doc = h.state().docs.active_doc().unwrap();
    assert_eq!(doc.path, root.join("db/changelog/01-crm.sql"));
    assert_eq!(doc.editor.caret_line_col(), (4, 13));

    // Find Usages on the definition: the foreign key, the read in the body, the ALTER TABLE.
    caret_at(&mut h, "crm.customers (", false);
    h.state_mut().run_insight_action(rc_editor::keymap::Action::FindUsages, egui::pos2(300.0, 300.0));
    h.run_steps(2);
    let (title, targets) = h.state().find_results.clone().expect("usages in the Find window");
    assert_eq!(title, "Usages of crm.customers");
    let places: Vec<(String, usize)> = targets.iter().map(|t| (t.path.file_name().unwrap().to_string_lossy().into_owned(), t.line)).collect();
    assert_eq!(places, [("01-crm.sql".to_string(), 10), ("02-orders.sql".into(), 5), ("02-orders.sql".into(), 11)]);
    assert!(targets[1].preview.contains("references crm.customers"));
}

#[test]
fn problems_while_typing_and_files_changed_on_disk() {
    let _lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().unwrap();
    let (_tmp, root) = workspace();
    let mut h = open(&root, home.path());
    let file = root.join("db/changelog/02-orders.sql");

    // A typo in the PL/pgSQL body is an error at the word, in the editor and in Problems.
    caret_at(&mut h, "    return p_customer;", false);
    type_text(&mut h, "selec 1;\n");
    settle(&mut h, "the PL/pgSQL error", |a| !a.docs.active_doc().unwrap().editor.diagnostics.is_empty());
    let doc = h.state().docs.active_doc().unwrap();
    let d = &doc.editor.diagnostics[0];
    let t = text(h.state());
    let word: String = t.chars().skip(d.start).take(d.end - d.start).collect();
    assert_eq!((word.as_str(), d.message.as_str()), ("selec", "syntax error at or near \"selec\""));
    assert!(h.state().insight.problems.get(&file).is_some_and(|p| p.len() == 1));
    // Fixed: the problem goes away.
    for _ in 0.."selec 1;\n".chars().count() {
        h.key_press(egui::Key::Backspace);
        h.run_steps(1);
    }
    settle(&mut h, "no problems", |a| a.docs.active_doc().unwrap().editor.diagnostics.is_empty());
    assert!(!h.state().insight.problems.contains_key(&file));

    // A new file on disk joins the index: its call resolves, and the function's usages list it.
    std::fs::write(root.join("db/changelog/03-checkout.sql"), "select orders_app.place('00000000-0000-0000-0000-000000000000');\n").unwrap();
    settle(&mut h, "the new file", |a| a.sql.files_for_test() == 3);
    caret_at(&mut h, "orders_app.place", true);
    h.state_mut().run_insight_action(rc_editor::keymap::Action::FindUsages, egui::pos2(300.0, 300.0));
    h.run_steps(2);
    // One usage opens directly.
    let doc = h.state().docs.active_doc().unwrap();
    assert_eq!(doc.path, root.join("db/changelog/03-checkout.sql"));
    assert_eq!(doc.editor.caret_line_col(), (0, 7));
}

#[test]
fn structure_lists_the_objects_a_file_creates() {
    let fa = rc_sql::analyze(ORDERS);
    let items: Vec<(String, &str)> = crate::sql_insight::structure(&fa).into_iter().map(|d| (d.key.qualified(), d.kind.label())).collect();
    assert_eq!(items, [("orders.orders".to_string(), "table"), ("orders_app.place".into(), "function")]);
}

#[test]
#[ignore = "Requires a GPU; writes target/snapshots/sql-dark.png"]
fn screens_sql() {
    let _lock = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&out).unwrap();
    let home = tempfile::tempdir().unwrap();
    let (_tmp, root) = workspace();
    let mut h = open(&root, home.path());
    h.state_mut().dock.show(crate::toolwindow::ToolWindow::Structure);
    h.run_steps(3);
    let pos = pos_of(h.state(), "crm.customers", 0, 5);
    editor_events(&mut h, &[EditorEvent::HoverAt { pos, screen: egui::pos2(520.0, 260.0) }]);
    h.run_steps(3);
    h.render().unwrap().save(out.join("sql-dark.png")).unwrap();
}
