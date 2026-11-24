//! `cargo run --release -p rc-sql --example corpus -- <folder> [name]`: analyses every .sql file
//! under the folder (read only) and prints counts, problems and, with a name, its usages.
use std::path::PathBuf;
use std::sync::Arc;

fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            if !p.file_name().is_some_and(|n| n == ".git" || n == "target") {
                walk(&p, out);
            }
        } else if rc_sql::is_sql_path(&p) {
            out.push(p);
        }
    }
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("folder"));
    let name = std::env::args().nth(2);
    let mut files = Vec::new();
    walk(&root, &mut files);
    let t = std::time::Instant::now();
    let analyses: Vec<(PathBuf, Arc<rc_sql::FileAnalysis>)> = files
        .iter()
        .map(|p| (p.strip_prefix(&root).unwrap().to_path_buf(), Arc::new(rc_sql::analyze(&std::fs::read_to_string(p).unwrap_or_default()))))
        .collect();
    let phase1 = t.elapsed();
    let index = rc_sql::Index::new(analyses);
    let objects = index.objects().len();
    let (defs, refs): (usize, usize) = index.files().map(|(_, f)| (f.definitions.len(), f.references.len())).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
    let resolved = index.files().flat_map(|(_, f)| f.references.iter()).filter(|r| !index.resolve(r).is_empty()).count();
    eprintln!("{} files, {objects} objects, {defs} definitions, {refs} references ({resolved} resolved), phase 1 {phase1:?}, total {:?}", files.len(), t.elapsed());
    for (path, f) in index.files() {
        let text = std::fs::read_to_string(root.join(path)).unwrap_or_default();
        for p in &f.problems {
            let line = text[..p.span.start.min(text.len())].matches('\n').count() + 1;
            println!("{}:{line}: {} [{}] {}", path.display(), if p.severity == rc_sql::Severity::Error { "error" } else { "warning" }, p.code, p.message);
        }
    }
    if let Some(name) = name {
        let (schema, n) = name.split_once('.').unwrap_or(("public", &name));
        for space in [rc_sql::Space::Relation, rc_sql::Space::Routine, rc_sql::Space::Type] {
            for (_, d) in index.objects().into_iter().filter(|(_, d)| d.key.space == space && d.key.schema == schema && d.key.name == n) {
                println!("== {} {}", d.kind.label(), d.signature());
                for (p, r) in index.usages(&d.key) {
                    let text = std::fs::read_to_string(root.join(p)).unwrap_or_default();
                    let line = text[..r.span.start].matches('\n').count() + 1;
                    println!("  {}:{line} {}", p.display(), r.kind.label());
                }
            }
        }
    }
}
