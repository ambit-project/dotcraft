//! What the analysis finds in a file: object definitions, references and problems. Offsets are
//! bytes of the file.

use crate::prepare::Changeset;
use std::fmt;
use std::ops::Range;

/// PostgreSQL's name spaces: tables, views, sequences and indexes share one (pg_class); routines,
/// types, schemas, triggers (per table) and extensions have their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Space {
    Relation,
    Routine,
    Type,
    Schema,
    Trigger,
    Extension,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ObjKind {
    Schema,
    Table,
    View,
    MaterializedView,
    Function,
    Procedure,
    Type,
    Domain,
    Sequence,
    Index,
    Trigger,
    Extension,
}

impl ObjKind {
    pub fn label(self) -> &'static str {
        match self {
            ObjKind::Schema => "schema",
            ObjKind::Table => "table",
            ObjKind::View => "view",
            ObjKind::MaterializedView => "materialized view",
            ObjKind::Function => "function",
            ObjKind::Procedure => "procedure",
            ObjKind::Type => "type",
            ObjKind::Domain => "domain",
            ObjKind::Sequence => "sequence",
            ObjKind::Index => "index",
            ObjKind::Trigger => "trigger",
            ObjKind::Extension => "extension",
        }
    }
}

/// What a reference does with its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefKind {
    Call,
    Read,
    Write,
    Type,
    ForeignKey,
    Trigger,
    IndexOn,
    Alter,
    Drop,
    Grant,
    Comment,
}

impl RefKind {
    pub fn label(self) -> &'static str {
        match self {
            RefKind::Call => "call",
            RefKind::Read => "read",
            RefKind::Write => "write",
            RefKind::Type => "type",
            RefKind::ForeignKey => "foreign key",
            RefKind::Trigger => "trigger",
            RefKind::IndexOn => "index",
            RefKind::Alter => "alter",
            RefKind::Drop => "drop",
            RefKind::Grant => "grant",
            RefKind::Comment => "comment",
        }
    }
}

/// The identity of an object; routines add their input argument types (`uuid,int4`), triggers
/// their table.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub space: Space,
    pub schema: String,
    pub name: String,
    pub signature: Option<String>,
}

impl Key {
    pub fn new(space: Space, schema: &str, name: &str) -> Key {
        Key { space, schema: schema.to_string(), name: name.to_string(), signature: None }
    }

    pub fn qualified(&self) -> String {
        if self.schema.is_empty() { self.name.clone() } else { format!("{}.{}", self.schema, self.name) }
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.signature, self.space) {
            (Some(sig), Space::Routine) => write!(f, "{}({sig})", self.qualified()),
            _ => f.write_str(&self.qualified()),
        }
    }
}

/// A name as written: `schema` is None when it was not qualified.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct QName {
    pub schema: Option<String>,
    pub name: String,
}

impl fmt::Display for QName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.schema {
            Some(s) => write!(f, "{s}.{}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    /// As written (empty for view columns).
    pub ty: String,
    pub not_null: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: Option<String>,
    /// in, out, inout, variadic, table.
    pub mode: &'static str,
    pub ty: String,
    pub has_default: bool,
}

impl Param {
    pub fn is_input(&self) -> bool {
        matches!(self.mode, "in" | "inout" | "variadic")
    }
}

impl fmt::Display for Param {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        if self.mode != "in" {
            parts.push(self.mode.to_ascii_uppercase());
        }
        if let Some(n) = &self.name {
            parts.push(n.clone());
        }
        parts.push(self.ty.clone());
        f.write_str(&parts.join(" "))
    }
}

/// One definition or change of an object (CREATE, CREATE OR REPLACE, ALTER TABLE).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    pub key: Key,
    pub kind: ObjKind,
    /// The name in the statement.
    pub name: Range<usize>,
    pub statement: Range<usize>,
    pub is_alter: bool,
    pub columns: Vec<Column>,
    pub dropped_columns: Vec<String>,
    pub params: Vec<Param>,
    pub returns: Option<String>,
    pub language: Option<String>,
    /// `author:id` of the Liquibase changeset.
    pub changeset: Option<String>,
    /// The changeset's `--comment:` text.
    pub comment: String,
    /// Indexes and triggers: their table.
    pub table: Option<QName>,
}

impl Definition {
    /// `orders_app.place(p_id uuid, INOUT p_version bigint)` / `… RETURNS int` / `orders.t`.
    pub fn signature(&self) -> String {
        let mut s = self.key.qualified();
        if matches!(self.kind, ObjKind::Function | ObjKind::Procedure) {
            s.push('(');
            s.push_str(&self.params.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", "));
            s.push(')');
            if let Some(r) = &self.returns {
                s.push_str(" RETURNS ");
                s.push_str(r);
            }
        }
        s
    }

    /// Input parameters: how many a call must pass at least and at most (None: any number).
    pub fn arity(&self) -> (usize, Option<usize>) {
        let inputs: Vec<&Param> = self.params.iter().filter(|p| p.is_input()).collect();
        let required = inputs.iter().filter(|p| !p.has_default && p.mode != "variadic").count();
        let max = if inputs.iter().any(|p| p.mode == "variadic") { None } else { Some(inputs.len()) };
        (required, max)
    }
}

/// A use of an object by name, resolved by the [`crate::Index`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub name: QName,
    pub space: Space,
    pub kind: RefKind,
    /// The name in the file (qualified: from the schema to the end of the name).
    pub span: Range<usize>,
    /// The object whose definition contains the reference.
    pub from: Option<Key>,
    /// Calls: the number of arguments.
    pub args: Option<usize>,
    /// The schemas an unqualified name is looked up in.
    pub search_path: Vec<String>,
    /// Found in the constant text of dynamic SQL (`EXECUTE format('…')`).
    pub dynamic: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub severity: Severity,
    pub message: String,
    pub span: Range<usize>,
    /// `syntax` or `plpgsql`.
    pub code: &'static str,
}

/// The analysis of one file, independent of the other files.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileAnalysis {
    pub definitions: Vec<Definition>,
    pub references: Vec<Reference>,
    pub problems: Vec<Problem>,
    pub changesets: Vec<Changeset>,
    /// Byte offset of each line's start.
    pub line_starts: Vec<usize>,
}

impl FileAnalysis {
    /// 0-based line and byte column of byte `offset`.
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let line = self.line_starts.partition_point(|&s| s <= offset).saturating_sub(1);
        (line, offset - self.line_starts.get(line).copied().unwrap_or(0))
    }
}
