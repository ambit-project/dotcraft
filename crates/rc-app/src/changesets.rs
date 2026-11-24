//! New Liquibase changesets of our PostgreSQL projects, like their `db/scripts/new-ddl.sh`,
//! `new-data.sh` and `new-module-routine.sh`: a dated DDL or data changeset
//! (`changelog/<yyyy>/1-ddl|2-data/<yyyymmdd>-<nn>-<slug>.sql`, the next free number of the day)
//! or a module's view, procedure or function (`changelog/modules/<nn>-<module>/<kind>s/
//! <schema>.<name>.sql`, `runOnChange:true`), each recorded in `CHANGELOG.md` under today's
//! heading.
//!
//! What differs between projects is read from the project itself: the author of its changesets
//! (or `CHANGESET_AUTHOR`), the role its module objects switch to (`SET LOCAL ROLE …_owner;`)
//! and its modules.

use rc_kit::i18n::{tr, trf};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A `db/` folder of Liquibase formatted SQL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbProject {
    /// The folder with `changelog/` and `CHANGELOG.md`.
    pub dir: PathBuf,
    /// The id before `:` in `--changeset`.
    pub author: String,
    /// What module objects switch to: `SET LOCAL ROLE <owner>;`.
    pub owner: String,
    /// `changelog/modules/<nn>-<module>` folder names, in order.
    pub modules: Vec<String>,
}

/// What to create.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Ddl,
    Data,
    View,
    Procedure,
    Function,
}

impl Kind {
    pub const ALL: [Kind; 5] = [Kind::Ddl, Kind::Data, Kind::View, Kind::Procedure, Kind::Function];

    pub fn title(self) -> &'static str {
        match self {
            Kind::Ddl => "DDL",
            Kind::Data => "Data",
            Kind::View => "View",
            Kind::Procedure => "Procedure",
            Kind::Function => "Function",
        }
    }

    pub fn is_routine(self) -> bool {
        matches!(self, Kind::View | Kind::Procedure | Kind::Function)
    }

    fn folder(self) -> &'static str {
        match self {
            Kind::Ddl => "1-ddl",
            Kind::Data => "2-data",
            Kind::View => "views",
            Kind::Procedure => "procedures",
            Kind::Function => "functions",
        }
    }

    fn singular(self) -> &'static str {
        match self {
            Kind::View => "view",
            Kind::Procedure => "procedure",
            Kind::Function => "function",
            Kind::Ddl => "ddl",
            Kind::Data => "data",
        }
    }
}

fn is_db(dir: &Path) -> bool {
    let changelog = dir.join("changelog");
    changelog.is_dir() && (dir.join("CHANGELOG.md").is_file() || dir.join("changelog.yaml").is_file() || changelog.join("modules").is_dir())
}

/// The `db/` folder around `path`: an ancestor that is one, or that has one in `db/`. Cheap
/// (a few metadata calls): menus ask it every frame.
pub fn locate(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() { path } else { path.parent()? };
    start.ancestors().find_map(|a| if is_db(a) { Some(a.to_path_buf()) } else { Some(a.join("db")).filter(|d| is_db(d)) })
}

/// The `db/` project around `path`, read from its files.
pub fn find(path: &Path) -> Option<DbProject> {
    Some(load(&locate(path)?))
}

/// The SQL files under `dir` (one level of year or module folders deep enough for the files).
fn sql_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() && depth > 0 {
            sql_files(&p, depth - 1, out);
        } else if p.extension().is_some_and(|x| x == "sql") {
            out.push(p);
        }
    }
}

/// The most common value, the first seen winning a tie.
fn most_common(values: Vec<String>) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for v in &values {
        *counts.entry(v).or_default() += 1;
    }
    let best = counts.values().copied().max()?;
    values.iter().find(|v| counts[v.as_str()] == best).cloned()
}

pub fn load(dir: &Path) -> DbProject {
    let changelog = dir.join("changelog");
    let mut modules: Vec<String> = std::fs::read_dir(changelog.join("modules"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.len() > 3 && n.as_bytes()[..2].iter().all(u8::is_ascii_digit) && n.as_bytes()[2] == b'-')
        .collect();
    modules.sort();
    let mut files = Vec::new();
    sql_files(&changelog, 3, &mut files);
    // The newest files first: a project that changed its author keeps the new one.
    files.sort();
    files.reverse();
    let mut authors = Vec::new();
    let mut owners = Vec::new();
    for f in files.iter().take(400) {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        for line in text.lines().take(40) {
            if let Some(rest) = line.strip_prefix("--changeset ")
                && let Some((author, _)) = rest.split_once(':')
            {
                authors.push(author.trim().to_string());
            }
            if let Some(role) = line.trim().strip_prefix("SET LOCAL ROLE ").and_then(|r| r.strip_suffix(';')) {
                owners.push(role.trim().to_string());
                break;
            }
        }
    }
    let author = std::env::var("CHANGESET_AUTHOR").ok().filter(|a| !a.trim().is_empty()).or_else(|| most_common(authors)).unwrap_or_else(|| {
        // A project without changesets yet: the repository's name.
        let repo = dir.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let name: String = repo.trim_start_matches("ambit.").trim_end_matches(".app").chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if name.is_empty() { "dev".into() } else { name }
    });
    let owner = most_common(owners).unwrap_or_else(|| "${role_prefix}_owner".into());
    DbProject { dir: dir.to_path_buf(), author, owner, modules }
}

/// Lower-case ASCII words joined with `-` (Polish letters transliterated).
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let c = match c {
            'ą' | 'Ą' => 'a',
            'ć' | 'Ć' => 'c',
            'ę' | 'Ę' => 'e',
            'ł' | 'Ł' => 'l',
            'ń' | 'Ń' => 'n',
            'ó' | 'Ó' => 'o',
            'ś' | 'Ś' => 's',
            'ź' | 'Ź' | 'ż' | 'Ż' => 'z',
            c => c.to_ascii_lowercase(),
        };
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// Today's local date as (year, month, day).
pub fn today() -> (i32, u32, u32) {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    #[cfg(unix)]
    {
        let t = secs as libc::time_t;
        // SAFETY: localtime_r fills the struct it is given.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if !unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return (tm.tm_year + 1900, tm.tm_mon as u32 + 1, tm.tm_mday as u32);
        }
    }
    // H. Hinnant's civil_from_days (UTC).
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((yoe + era * 400 + i64::from(m <= 2)) as i32, m, d)
}

/// What the form asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    /// DDL and data: the feature; routines: what the change does. Becomes `--comment:`.
    pub comment: String,
    /// Data only: `context:dev` (seeds that never reach production).
    pub dev: bool,
    /// Routines: `<nn>-<module>`.
    pub module: String,
    /// Routines: `name` (in `<module>_app`) or `schema.name`.
    pub name: String,
}

/// A created file and where the caret goes in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    pub path: PathBuf,
    /// Char offset of the caret.
    pub caret: usize,
}

/// `schema.object` of a routine's name, or why it is not one.
pub fn routine_name(module: &str, name: &str) -> Result<(String, String), String> {
    let module_name = module.get(3..).unwrap_or(module);
    let mut n = name.trim().to_lowercase();
    if !n.contains('.') {
        n = format!("{module_name}_app.{n}");
    }
    let ok = |p: &str| !p.is_empty() && p.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_') && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    match n.split_once('.') {
        Some((s, o)) if ok(s) && ok(o) => Ok((s.to_string(), o.to_string())),
        _ => Err(trf("Letters, digits and underscores only, e.g. client_list or {schema}.client_list", &[("schema", &format!("{module_name}_app"))])),
    }
}

/// The path a request would create (for the form's preview), or why it can't.
pub fn target(db: &DbProject, kind: Kind, req: &Request, date: (i32, u32, u32)) -> Result<PathBuf, String> {
    let changelog = db.dir.join("changelog");
    if kind.is_routine() {
        if req.module.is_empty() {
            return Err(tr("Pick a module").into());
        }
        let (schema, object) = routine_name(&req.module, &req.name)?;
        return Ok(changelog.join("modules").join(&req.module).join(kind.folder()).join(format!("{schema}.{object}.sql")));
    }
    let slug = slug(&req.comment);
    if slug.is_empty() {
        return Err(tr("The feature name must contain letters or digits").into());
    }
    let day = format!("{:04}{:02}{:02}", date.0, date.1, date.2);
    let dir = changelog.join(format!("{:04}", date.0)).join(kind.folder());
    let last = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let rest = name.strip_prefix(&day)?.strip_prefix('-')?;
            (rest.len() > 3 && rest.as_bytes()[2] == b'-' && name.ends_with(".sql")).then(|| rest[..2].parse::<u32>().ok()).flatten()
        })
        .max()
        .unwrap_or(0);
    Ok(dir.join(format!("{day}-{:02}-{slug}.sql", last + 1)))
}

/// `--comment:` lines of `text`, wrapped at 108 columns like the scripts.
fn comment_lines(text: &str) -> String {
    let mut out = String::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > 108 {
            out.push_str(&format!("--comment: {line}\n"));
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    out.push_str(&format!("--comment: {line}\n"));
    out
}

/// Creates the changeset and records it in `CHANGELOG.md`.
pub fn create(db: &DbProject, kind: Kind, req: &Request, date: (i32, u32, u32)) -> Result<Created, String> {
    let comment = req.comment.trim();
    if comment.is_empty() {
        return Err(if kind.is_routine() { tr("Say what the change does (the comment)") } else { tr("Name the feature") }.into());
    }
    if req.dev && kind != Kind::Data {
        return Err(tr("Development only applies to data changesets").into());
    }
    let path = target(db, kind, req, date)?;
    if path.exists() {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        return Err(if kind.is_routine() {
            trf("{file} already exists - change it in place (runOnChange:true runs it again)", &[("file", &name)])
        } else {
            trf("{file} already exists", &[("file", &name)])
        });
    }
    let author = &db.author;
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let rel = path.strip_prefix(db.dir.join("changelog")).unwrap_or(&path).to_string_lossy().replace('\\', "/");
    let (content, heading) = if kind.is_routine() {
        let (schema, object) = routine_name(&req.module, &req.name)?;
        let id = format!("{}-{}-{}", kind.singular(), schema.replace('_', "-"), object.replace('_', "-"));
        let body = match kind {
            Kind::View => format!("CREATE OR REPLACE VIEW {schema}.{object} AS\nSELECT\n    $0-- columns: new ones only at the end\nFROM\nWHERE;\n"),
            Kind::Procedure => format!(
                "CREATE OR REPLACE PROCEDURE {schema}.{object}()\nLANGUAGE plpgsql\nSECURITY DEFINER\nSET search_path = pg_catalog, pg_temp\nAS $$\nBEGIN$0\nEND\n$$;\n"
            ),
            _ => format!(
                "CREATE OR REPLACE FUNCTION {schema}.{object}()\n    RETURNS TABLE ()\n    LANGUAGE sql\n    STABLE\n    SECURITY DEFINER\n    SET search_path = pg_catalog, pg_temp\nAS $$$0\n$$;\n"
            ),
        };
        let content = format!(
            "--liquibase formatted sql\n\n--changeset {author}:{id} runOnChange:true splitStatements:false\n{}SET LOCAL ROLE {};\n\n{body}\nRESET ROLE;\n",
            comment_lines(comment),
            db.owner
        );
        let module_name = req.module.get(3..).unwrap_or(&req.module);
        (content, format!("{module_name}: {} {schema}.{object}", kind.singular()))
    } else {
        let context = if req.dev { " context:dev" } else { "" };
        let roles = if db.owner.contains("${role_prefix}") { "-- roles: ${role_prefix}\n" } else { "" };
        let rollback = if kind == Kind::Data { "--rollback \n" } else { "" };
        let content = format!("--liquibase formatted sql\n\n--changeset {author}:{stem}{context} splitStatements:false\n--comment: {comment}\n{roles}\n$0{rollback}");
        (content, comment.to_string())
    };
    let caret = content.find("$0").map_or(content.chars().count(), |at| content[..at].chars().count());
    let content = content.replacen("$0", "", 1);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, content).map_err(|e| e.to_string())?;
    let day = format!("{:04}-{:02}-{:02}", date.0, date.1, date.2);
    changelog_add(&db.dir, &day, &heading, &rel).map_err(|e| e.to_string())?;
    Ok(Created { path, caret })
}

/// Adds "- `<rel>`" to `CHANGELOG.md` under "## <day> (<heading>)", after that section's last
/// entry; creates the file or the section when missing (`lib-changelog.sh`).
pub fn changelog_add(db: &Path, day: &str, heading: &str, rel: &str) -> std::io::Result<()> {
    let path = db.join("CHANGELOG.md");
    let heading = format!("## {day} ({heading})");
    let entry = format!("- `{rel}`");
    let mut text = std::fs::read_to_string(&path).unwrap_or_else(|_| "# Database changelog\n\nOne entry per file; the path is relative to `changelog/`.\n".into());
    let lines: Vec<&str> = text.lines().collect();
    if let Some(start) = lines.iter().position(|l| *l == heading) {
        let mut at = start;
        for (i, l) in lines.iter().enumerate().skip(start + 1) {
            if l.starts_with("## ") {
                break;
            }
            if l.starts_with("- ") {
                at = i;
            }
        }
        let mut out: Vec<&str> = lines.clone();
        out.insert(at + 1, &entry);
        text = out.join("\n") + "\n";
    } else {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("\n{heading}\n\n{entry}\n"));
    }
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests_support {
    use std::path::PathBuf;

    /// A project like the neon one: one module, an older changeset, a module procedure.
    pub fn project() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("ambit.shop.app/db");
        std::fs::create_dir_all(db.join("changelog/2026/1-ddl")).unwrap();
        std::fs::create_dir_all(db.join("changelog/modules/10-crm/procedures")).unwrap();
        std::fs::create_dir_all(db.join("changelog/modules/00-platform/views")).unwrap();
        std::fs::create_dir_all(db.join("scripts")).unwrap();
        std::fs::write(db.join("changelog/2026/1-ddl/20261101-01-crm-create-tables.sql"), "--liquibase formatted sql\n\n--changeset shop:20261101-01-crm-create-tables splitStatements:false\n--comment: Tables\n").unwrap();
        std::fs::write(
            db.join("changelog/modules/10-crm/procedures/crm_app.client_add.sql"),
            "--liquibase formatted sql\n\n--changeset shop:procedure-crm-app-client-add runOnChange:true splitStatements:false\n--comment: Adds\nSET LOCAL ROLE ${role_prefix}_owner;\n",
        )
        .unwrap();
        std::fs::write(db.join("CHANGELOG.md"), "# Database changelog\n\nOne entry per file; the path is relative to `changelog/`.\n\n## 2026-11-01\n\n- `2026/1-ddl/20261101-01-crm-create-tables.sql`\n").unwrap();
        (tmp, db)
    }

}

#[cfg(test)]
mod tests {
    use super::tests_support::project;
    use super::*;

    #[test]
    fn the_project_is_found_and_read_from_its_files() {
        let (_tmp, db) = project();
        let from_repo = find(&db.parent().unwrap().join("src")).or_else(|| find(db.parent().unwrap())).unwrap();
        assert_eq!(from_repo.dir, db);
        let p = find(&db.join("changelog/modules/10-crm/procedures/crm_app.client_add.sql")).unwrap();
        assert_eq!((p.author.as_str(), p.owner.as_str()), ("shop", "${role_prefix}_owner"));
        assert_eq!(p.modules, ["00-platform", "10-crm"]);
        assert!(find(Path::new("/proc/self")).is_none());
    }

    #[test]
    fn ddl_and_data_changesets_take_the_next_number_of_the_day() {
        let (_tmp, db) = project();
        let p = load(&db);
        let day = (2026, 11, 24);
        let req = Request { comment: "Zamówienia: pozycje i rabaty".into(), ..Default::default() };
        let first = create(&p, Kind::Ddl, &req, day).unwrap();
        assert_eq!(first.path, db.join("changelog/2026/1-ddl/20261124-01-zamowienia-pozycje-i-rabaty.sql"));
        let text = std::fs::read_to_string(&first.path).unwrap();
        assert_eq!(text, "--liquibase formatted sql\n\n--changeset shop:20261124-01-zamowienia-pozycje-i-rabaty splitStatements:false\n--comment: Zamówienia: pozycje i rabaty\n-- roles: ${role_prefix}\n\n");
        assert_eq!(first.caret, text.chars().count());
        let second = create(&p, Kind::Ddl, &Request { comment: "Indexes".into(), ..Default::default() }, day).unwrap();
        assert!(second.path.ends_with("20261124-02-indexes.sql"));
        let data = create(&p, Kind::Data, &Request { comment: "Seed admins".into(), dev: true, ..Default::default() }, day).unwrap();
        assert_eq!(data.path, db.join("changelog/2026/2-data/20261124-01-seed-admins.sql"));
        let text = std::fs::read_to_string(&data.path).unwrap();
        assert!(text.contains("--changeset shop:20261124-01-seed-admins context:dev splitStatements:false\n") && text.ends_with("\n--rollback \n"), "{text}");
        assert!(create(&p, Kind::Ddl, &Request { dev: true, comment: "x".into(), ..Default::default() }, day).is_err());
        assert!(create(&p, Kind::Ddl, &Request { comment: "!!!".into(), ..Default::default() }, day).is_err());
        let log = std::fs::read_to_string(db.join("CHANGELOG.md")).unwrap();
        assert!(log.ends_with(
            "## 2026-11-24 (Zamówienia: pozycje i rabaty)\n\n- `2026/1-ddl/20261124-01-zamowienia-pozycje-i-rabaty.sql`\n\n## 2026-11-24 (Indexes)\n\n- `2026/1-ddl/20261124-02-indexes.sql`\n\n## 2026-11-24 (Seed admins)\n\n- `2026/2-data/20261124-01-seed-admins.sql`\n"
        ), "{log}");
    }

    #[test]
    fn module_routines_follow_the_module_conventions() {
        let (_tmp, db) = project();
        let p = load(&db);
        let day = (2026, 11, 24);
        let req = Request { comment: "Lists a client's orders with their totals, newest first; only the client's own people see them.".into(), module: "10-crm".into(), name: "Order_List".into(), ..Default::default() };
        let view = create(&p, Kind::View, &req, day).unwrap();
        assert_eq!(view.path, db.join("changelog/modules/10-crm/views/crm_app.order_list.sql"));
        let text = std::fs::read_to_string(&view.path).unwrap();
        assert!(text.starts_with("--liquibase formatted sql\n\n--changeset shop:view-crm-app-order-list runOnChange:true splitStatements:false\n--comment: Lists"), "{text}");
        assert!(text.contains("\nSET LOCAL ROLE ${role_prefix}_owner;\n\nCREATE OR REPLACE VIEW crm_app.order_list AS\nSELECT\n    -- columns") && text.ends_with("\nRESET ROLE;\n"), "{text}");
        assert!(text.lines().all(|l| l.len() <= 120));
        assert!(text[..text.char_indices().nth(view.caret).unwrap().0].ends_with("SELECT\n    "));
        let proc = create(&p, Kind::Procedure, &Request { comment: "Validates".into(), module: "10-crm".into(), name: "crm_internal.client_validate".into(), ..Default::default() }, day).unwrap();
        assert!(proc.path.ends_with("modules/10-crm/procedures/crm_internal.client_validate.sql"));
        assert!(std::fs::read_to_string(&proc.path).unwrap().contains("--changeset shop:procedure-crm-internal-client-validate "));
        // The same object again: change it in place.
        assert!(create(&p, Kind::View, &req, day).unwrap_err().contains("already exists"));
        assert!(create(&p, Kind::Function, &Request { comment: "x".into(), module: "10-crm".into(), name: "bad name".into(), ..Default::default() }, day).is_err());
        let log = std::fs::read_to_string(db.join("CHANGELOG.md")).unwrap();
        assert!(log.contains("## 2026-11-24 (crm: view crm_app.order_list)\n\n- `modules/10-crm/views/crm_app.order_list.sql`\n"), "{log}");
    }

    #[test]
    fn entries_join_an_existing_section() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("CHANGELOG.md"), "# Database changelog\n\n## 2026-11-24 (A)\n\n- `a.sql`\n- `b.sql`\n\n## 2026-11-25 (B)\n\n- `c.sql`\n").unwrap();
        changelog_add(tmp.path(), "2026-11-24", "A", "new.sql").unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path().join("CHANGELOG.md")).unwrap(), "# Database changelog\n\n## 2026-11-24 (A)\n\n- `a.sql`\n- `b.sql`\n- `new.sql`\n\n## 2026-11-25 (B)\n\n- `c.sql`\n");
    }

    #[test]
    fn slugs_and_dates() {
        assert_eq!(slug("  Zażółć gęślą JAŹŃ -- v2 "), "zazolc-gesla-jazn-v2");
        let (y, m, d) = today();
        assert!(y >= 2026 && (1..=12).contains(&m) && (1..=31).contains(&d));
    }
}

#[cfg(test)]
mod parity {
    use super::*;

    /// `PARITY_DB=<copy of a db/> cargo test … -- --ignored changesets::parity`: the same four
    /// changesets as `new-ddl.sh`, `new-data.sh --dev` and two `new-module-routine.sh` runs.
    #[test]
    #[ignore = "compares with the db/scripts of a real project; needs PARITY_DB"]
    fn same_files_as_the_scripts() {
        let db = PathBuf::from(std::env::var("PARITY_DB").expect("PARITY_DB"));
        let p = load(&db);
        let day = today();
        create(&p, Kind::Ddl, &Request { comment: "Zamówienia: pozycje i rabaty".into(), ..Default::default() }, day).unwrap();
        create(&p, Kind::Data, &Request { comment: "Seed admins".into(), dev: true, ..Default::default() }, day).unwrap();
        create(&p, Kind::View, &Request { comment: "Lists the orders.".into(), module: "05-scheduling".into(), name: "order_list".into(), ..Default::default() }, day).unwrap();
        create(&p, Kind::Procedure, &Request { comment: "Checks an order.".into(), module: "05-scheduling".into(), name: "crm_internal.order_check".into(), ..Default::default() }, day).unwrap();
    }
}

#[cfg(test)]
mod ui_tests {
    use super::tests_support::project;
    use super::*;
    use crate::ui::explorer::{ExplorerAction, NodeKind};
    use egui_kittest::kittest::Queryable;

    #[test]
    fn the_explorer_offers_a_changeset_inside_a_db_project_only() {
        let (_tmp, db) = project();
        let procedures = db.join("changelog/modules/10-crm/procedures");
        assert_eq!(crate::ui::explorer::menu_actions_for_test(NodeKind::Dir(procedures.clone()), tr("New Changeset…")), [ExplorerAction::NewChangeset(procedures)]);
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(crate::ui::explorer::menu_actions_for_test(NodeKind::Dir(elsewhere.path().to_path_buf()), tr("New Changeset…")).is_empty());
    }

    #[test]
    fn the_dialog_creates_opens_and_records_the_changeset() {
        let _lock = crate::screens::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempfile::tempdir().unwrap();
        let (_tmp, db) = project();
        let mut app = crate::screens::test_app(home.path(), true);
        app.settings.roslyn_path = Some("/nonexistent".into());
        app.open_workspace(db.parent().unwrap());
        app.new_changeset(&db.join("changelog/modules/10-crm/procedures"));
        let mut h = crate::screens::harness(app, egui::vec2(1280.0, 800.0));
        h.run_steps(3);
        {
            let Some(crate::ui::dialogs::Dialog::NewChangeset(form)) = &mut h.state_mut().dialog else { panic!("the dialog") };
            assert_eq!((form.kind, form.req.module.as_str(), form.db.author.as_str()), (Kind::Procedure, "10-crm", "shop"));
            form.req.name = "order_close".into();
            form.req.comment = "Closes an order.".into();
        }
        h.run_steps(2);
        assert!(h.query_by_label_contains("crm_app.order_close.sql").is_some(), "the path is previewed");
        h.get_by_label(tr("Create")).click();
        h.run_steps(3);
        let file = db.join("changelog/modules/10-crm/procedures/crm_app.order_close.sql");
        assert!(h.state().dialog.is_none());
        let doc = h.state().docs.active_doc().unwrap();
        assert_eq!(doc.path, file);
        assert_eq!(doc.editor.caret_line_col().0, 11, "the caret in the body (after BEGIN)");
        assert!(std::fs::read_to_string(db.join("CHANGELOG.md")).unwrap().contains("- `modules/10-crm/procedures/crm_app.order_close.sql`"));
        assert!(h.state().notices.iter().any(|n| n.text.contains("crm_app.order_close.sql")));
    }
}

#[cfg(test)]
mod screens {
    use super::tests_support::project;

    #[test]
    #[ignore = "Requires a GPU; writes target/snapshots/new-changeset-dark.png"]
    fn screens_new_changeset() {
        let _lock = crate::screens::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
        std::fs::create_dir_all(&out).unwrap();
        let home = tempfile::tempdir().unwrap();
        let (_tmp, db) = project();
        let mut app = crate::screens::test_app(home.path(), true);
        app.settings.roslyn_path = Some("/nonexistent".into());
        app.open_workspace(db.parent().unwrap());
        app.new_changeset(&db.join("changelog/modules/10-crm/views"));
        if let Some(crate::ui::dialogs::Dialog::NewChangeset(form)) = &mut app.dialog {
            form.req.name = "order_list".into();
            form.req.comment = "Lists a client's orders with their totals, newest first.".into();
        }
        let mut h = crate::screens::harness(app, egui::vec2(1280.0, 800.0));
        h.run_steps(4);
        h.render().unwrap().save(out.join("new-changeset-dark.png")).unwrap();
    }
}
