//! Phase 2: the files of a folder linked together. Names resolve like PostgreSQL resolves them:
//! a qualified name in its schema, an unqualified one along the search path; calls pick the
//! overloads whose parameters fit the number of arguments.

use crate::model::{Definition, FileAnalysis, Key, ObjKind, Reference, Space};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Where a definition or reference is: the file and its byte range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    pub span: std::ops::Range<usize>,
}

/// What is at a position of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hit {
    /// The name of a definition.
    Definition(Key),
    /// A reference and the objects it resolves to (none: not in the folder, e.g. a built-in).
    Reference(Reference, Vec<Key>),
}

#[derive(Default)]
pub struct Index {
    files: Vec<(PathBuf, Arc<FileAnalysis>)>,
    /// (space, schema, name) → (file, definition) in file order.
    objects: HashMap<(Space, String, String), Vec<(usize, usize)>>,
}

impl Index {
    pub fn new(files: Vec<(PathBuf, Arc<FileAnalysis>)>) -> Index {
        let mut files = files;
        // Changelog order: by path (Liquibase folders are numbered).
        files.sort_by(|a, b| a.0.cmp(&b.0));
        let mut objects: HashMap<(Space, String, String), Vec<(usize, usize)>> = HashMap::new();
        for (fi, (_, fa)) in files.iter().enumerate() {
            for (di, d) in fa.definitions.iter().enumerate() {
                objects.entry((d.key.space, d.key.schema.clone(), d.key.name.clone())).or_default().push((fi, di));
            }
        }
        Index { files, objects }
    }

    pub fn files(&self) -> impl Iterator<Item = (&Path, &FileAnalysis)> {
        self.files.iter().map(|(p, f)| (p.as_path(), f.as_ref()))
    }

    fn file(&self, path: &Path) -> Option<(usize, &FileAnalysis)> {
        self.files.iter().position(|(p, _)| p == path).map(|i| (i, self.files[i].1.as_ref()))
    }

    fn candidates(&self, space: Space, schema: &str, name: &str) -> &[(usize, usize)] {
        self.objects.get(&(space, schema.to_string(), name.to_string())).map_or(&[], Vec::as_slice)
    }

    /// The objects a reference stands for (distinct keys, overloads that fit first).
    pub fn resolve(&self, r: &Reference) -> Vec<Key> {
        // A type can also be a table's row type; `%TYPE` names a relation.
        let spaces = if r.space == Space::Type { vec![Space::Type, Space::Relation] } else { vec![r.space] };
        let schemas: Vec<&str> = match &r.name.schema {
            Some(s) => vec![s.as_str()],
            None if matches!(r.space, Space::Schema | Space::Extension) => vec![""],
            None => {
                let mut v: Vec<&str> = r.search_path.iter().map(String::as_str).collect();
                if !v.contains(&"public") {
                    v.push("public");
                }
                v
            }
        };
        for space in spaces {
            for schema in &schemas {
                let found = self.candidates(space, schema, &r.name.name);
                if found.is_empty() {
                    continue;
                }
                let defs: Vec<&Definition> = found.iter().map(|&(f, d)| &self.files[f].1.definitions[d]).collect();
                let mut keys: Vec<Key> = Vec::new();
                if let (Space::Routine, Some(n)) = (space, r.args) {
                    for d in &defs {
                        let (min, max) = d.arity();
                        if n >= min && max.is_none_or(|m| n <= m) && !keys.contains(&d.key) {
                            keys.push(d.key.clone());
                        }
                    }
                }
                if keys.is_empty() {
                    for d in &defs {
                        if !keys.contains(&d.key) {
                            keys.push(d.key.clone());
                        }
                    }
                }
                return keys;
            }
        }
        Vec::new()
    }

    /// Every definition and change of `key`, in changelog order.
    pub fn definitions(&self, key: &Key) -> Vec<(&Path, &Definition)> {
        self.candidates(key.space, &key.schema, &key.name)
            .iter()
            .map(|&(f, d)| (self.files[f].0.as_path(), &self.files[f].1.definitions[d]))
            .filter(|(_, d)| d.key == *key)
            .collect()
    }

    /// The defining statement: the latest CREATE (OR REPLACE), or the first change when the
    /// object is only altered in this folder.
    pub fn primary(&self, key: &Key) -> Option<(&Path, &Definition)> {
        let defs = self.definitions(key);
        let creates: Vec<_> = defs.iter().filter(|(_, d)| !d.is_alter).copied().collect();
        if matches!(key.space, Space::Routine) { creates.last().copied() } else { creates.first().copied() }.or_else(|| defs.first().copied())
    }

    /// Columns after every ALTER TABLE in order.
    pub fn columns(&self, key: &Key) -> Vec<crate::model::Column> {
        let mut columns: Vec<crate::model::Column> = Vec::new();
        for (_, d) in self.definitions(key) {
            if !d.is_alter {
                columns.clear();
            }
            columns.retain(|c| !d.dropped_columns.contains(&c.name));
            for c in &d.columns {
                columns.retain(|x| x.name != c.name);
                columns.push(c.clone());
            }
        }
        columns
    }

    /// Every reference that resolves to `key`, in file order.
    pub fn usages(&self, key: &Key) -> Vec<(&Path, &Reference)> {
        let mut out = Vec::new();
        for (path, fa) in &self.files {
            for r in &fa.references {
                if r.name.name == key.name && self.resolve(r).contains(key) {
                    out.push((path.as_path(), r));
                }
            }
        }
        out
    }

    /// The definition name or reference at byte `offset` of `path`.
    pub fn at(&self, path: &Path, offset: usize) -> Option<Hit> {
        let (_, fa) = self.file(path)?;
        let inside = |s: &std::ops::Range<usize>| s.start <= offset && offset <= s.end && s.start < s.end;
        if let Some(d) = fa.definitions.iter().find(|d| inside(&d.name)) {
            return Some(Hit::Definition(d.key.clone()));
        }
        // The innermost (shortest) reference around the offset.
        let r = fa.references.iter().filter(|r| inside(&r.span)).min_by_key(|r| r.span.len())?;
        Some(Hit::Reference(r.clone(), self.resolve(r)))
    }

    /// 0-based line and byte column of byte `offset` of `path`.
    pub fn line_col(&self, path: &Path, offset: usize) -> (usize, usize) {
        self.file(path).map_or((0, 0), |(_, f)| f.line_col(offset))
    }

    /// Every object defined in the folder (its primary definition), for search.
    pub fn objects(&self) -> Vec<(&Path, &Definition)> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (path, fa) in &self.files {
            for d in &fa.definitions {
                if !d.is_alter && d.kind != ObjKind::Schema && seen.insert(d.key.clone()) {
                    out.push((path.as_path(), d));
                }
            }
        }
        out
    }
}
