//! Phase 1 of the analysis: one file alone. Each Liquibase changeset (or the whole file) is
//! parsed by libpg_query into its JSON tree; CREATE statements become definitions, names in the
//! tree become references with what they do (an INSERT target is a write, a REFERENCES target
//! a foreign key…), PL/pgSQL bodies are read token by token ([`crate::body`]).
//!
//! A port of PG Studio's `analysis/extract.dart` (ambit-project/ambit-pg-studio).

use crate::body::scan_plpgsql_body;
use crate::ffi::{self, ParseError};
use crate::model::*;
use crate::prepare::{Changeset, prepare, restore};
use crate::tokens::{Run, Tokens};
use serde_json::Value as Json;

pub(crate) const BUILTIN_SCHEMAS: &[&str] = &["pg_catalog", "information_schema", "pg_toast"];
const TYPE_STOPS: &[&str] = &["not", "null", "default", "primary", "references", "unique", "check", "constraint", "generated", "collate"];
const RETURNS_STOPS: &[&str] = &[
    "language", "as", "immutable", "stable", "volatile", "security", "set", "strict", "called", "parallel", "cost", "rows", "leakproof", "window", "begin", "return",
    "support", "external",
];

/// Analyses one file's text.
pub fn analyze(text: &str) -> FileAnalysis {
    let prepared = prepare(text);
    let line_starts = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let mut out = FileAnalysis { changesets: prepared.changesets.clone(), line_starts, ..Default::default() };
    let chunks: Vec<(usize, usize, Option<&Changeset>)> = if prepared.changesets.is_empty() {
        vec![(0, prepared.sql.len(), None)]
    } else {
        prepared.changesets.iter().map(|c| (c.start, c.end, Some(c))).collect()
    };
    for (start, end, changeset) in chunks {
        let mut file = File { out: &mut out, properties: &prepared.properties, changeset, search_path: vec!["public".into()] };
        file.chunk(&prepared.sql[start..end], start);
    }
    out
}

/// Byte offset of the 1-based character position `cursor` in `text`.
fn byte_of_cursor(text: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return text.len() - text.trim_start().len();
    }
    text.char_indices().nth(cursor - 1).map_or(text.len(), |(b, _)| b)
}

/// The end of the word an error points at (the whole word is underlined).
fn error_end(text: &str, at: usize) -> usize {
    let rest = &text[at.min(text.len())..];
    let word = rest.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '"')).unwrap_or(rest.len());
    if word > 0 {
        return at + word;
    }
    at + rest.chars().next().map_or(0, char::len_utf8)
}

struct File<'a> {
    out: &'a mut FileAnalysis,
    properties: &'a [String],
    changeset: Option<&'a Changeset>,
    /// `SET search_path` of the changeset so far.
    search_path: Vec<String>,
}

impl File<'_> {
    /// One changeset (or the whole file), parsed alone so a syntax error stays inside it. When
    /// it does not parse, its statements are parsed one by one: the others still count.
    fn chunk(&mut self, sql: &str, base: usize) {
        if sql.trim().is_empty() {
            return;
        }
        match ffi::parse_json(sql) {
            Ok(tree) => self.statements(sql, base, &tree),
            Err(error) => match pg_query::split_with_scanner(sql) {
                Ok(parts) if parts.len() > 1 => {
                    for part in parts {
                        let offset = part.as_ptr() as usize - sql.as_ptr() as usize;
                        match ffi::parse_json(part) {
                            Ok(tree) => self.statements(part, base + offset, &tree),
                            Err(e) => self.syntax_error(part, base + offset, &e),
                        }
                    }
                }
                _ => self.syntax_error(sql, base, &error),
            },
        }
    }

    fn syntax_error(&mut self, sql: &str, base: usize, e: &ParseError) {
        let at = byte_of_cursor(sql, e.cursor);
        let end = error_end(sql, at).max(at);
        self.out.problems.push(Problem { severity: Severity::Error, message: e.message.clone(), span: base + at..base + end, code: "syntax" });
    }

    fn statements(&mut self, sql: &str, base: usize, tree: &Json) {
        let tokens = Tokens::new(sql);
        for raw in tree["stmts"].as_array().into_iter().flatten() {
            let start = raw["stmt_location"].as_u64().unwrap_or(0) as usize;
            let len = raw["stmt_len"].as_u64().unwrap_or(0) as usize;
            let end = if len == 0 { sql.len() } else { (start + len).min(sql.len()) };
            if let Some((ty, node)) = raw["stmt"].as_object().and_then(|o| o.iter().next()) {
                Stmt { file: self, sql, base, tokens: &tokens, ty, node, start, end }.run();
            }
        }
    }
}

fn names(list: &Json) -> Vec<String> {
    list.as_array().into_iter().flatten().filter_map(|n| n["String"]["sval"].as_str().map(str::to_string)).collect()
}

fn location(v: &Json) -> Option<usize> {
    v.as_i64().filter(|l| *l >= 0).map(|l| l as usize)
}

fn space_of_object_type(t: Option<&str>) -> Option<Space> {
    Some(match t? {
        "OBJECT_TABLE" | "OBJECT_VIEW" | "OBJECT_MATVIEW" | "OBJECT_SEQUENCE" | "OBJECT_INDEX" | "OBJECT_FOREIGN_TABLE" => Space::Relation,
        "OBJECT_FUNCTION" | "OBJECT_PROCEDURE" | "OBJECT_ROUTINE" | "OBJECT_AGGREGATE" => Space::Routine,
        "OBJECT_TYPE" | "OBJECT_DOMAIN" => Space::Type,
        "OBJECT_SCHEMA" => Space::Schema,
        "OBJECT_EXTENSION" => Space::Extension,
        _ => return None,
    })
}

/// One statement of a parsed text.
struct Stmt<'a, 'f> {
    file: &'a mut File<'f>,
    /// The parsed text, starting at byte `base` of the file.
    sql: &'a str,
    base: usize,
    tokens: &'a Tokens<'a>,
    ty: &'a str,
    node: &'a Json,
    start: usize,
    end: usize,
}

impl Stmt<'_, '_> {
    fn r(&self, name: &str) -> String {
        restore(name, self.file.properties)
    }

    fn schema(&self, v: &Json) -> String {
        v.as_str().map_or_else(|| "public".to_string(), |s| self.r(s))
    }

    fn qname(&self, parts: &[String]) -> QName {
        match parts {
            [.., schema, name] => QName { schema: Some(self.r(schema)), name: self.r(name) },
            [name] => QName { schema: None, name: self.r(name) },
            [] => QName { schema: None, name: String::new() },
        }
    }

    fn run(&mut self) {
        let node = self.node;
        match self.ty {
            "CreateSchemaStmt" => {
                if let Some(name) = node["schemaname"].as_str() {
                    let key = Key::new(Space::Schema, "", &self.r(name));
                    let run = self.tokens.name_after(&["schema"], self.start, self.end);
                    self.define(ObjKind::Schema, key, run, |_| {});
                }
            }
            "CreateStmt" => self.create_table(),
            "ViewStmt" => self.create_view(),
            "CreateTableAsStmt" => self.create_table_as(),
            "CreateFunctionStmt" => self.create_routine(),
            "IndexStmt" => self.create_index(),
            "CreateTrigStmt" => self.create_trigger(),
            "CompositeTypeStmt" => {
                let rv = &node["typevar"];
                let key = Key::new(Space::Type, &self.schema(&rv["schemaname"]), &self.r(rv["relname"].as_str().unwrap_or_default()));
                let run = self.run_at(&rv["location"]);
                let columns = self.columns(&node["coldeflist"]);
                self.define(ObjKind::Type, key.clone(), run, |d| d.columns = columns);
                self.walk_children(node, Some(&key), &["typevar"]);
            }
            "CreateEnumStmt" => {
                let n = names(&node["typeName"]);
                let schema = if n.len() > 1 { self.r(&n[0]) } else { "public".into() };
                let key = Key::new(Space::Type, &schema, &self.r(n.last().map_or("", String::as_str)));
                let values: Vec<Column> = names(&node["vals"]).into_iter().map(|v| Column { name: v, ty: String::new(), not_null: false }).collect();
                let run = self.tokens.name_after(&["type"], self.start, self.end);
                self.define(ObjKind::Type, key, run, |d| d.columns = values);
            }
            "CreateDomainStmt" => {
                let n = names(&node["domainname"]);
                let schema = if n.len() > 1 { self.r(&n[0]) } else { "public".into() };
                let key = Key::new(Space::Type, &schema, &self.r(n.last().map_or("", String::as_str)));
                let run = self.tokens.name_after(&["domain"], self.start, self.end);
                let base = self.type_text(&node["typeName"], TYPE_STOPS);
                self.define(ObjKind::Domain, key.clone(), run, |d| d.returns = base);
                self.walk_children(node, Some(&key), &["domainname"]);
            }
            "CreateSeqStmt" => {
                let rv = &node["sequence"];
                let key = Key::new(Space::Relation, &self.schema(&rv["schemaname"]), &self.r(rv["relname"].as_str().unwrap_or_default()));
                let run = self.run_at(&rv["location"]);
                self.define(ObjKind::Sequence, key, run, |_| {});
            }
            "CreateExtensionStmt" => {
                let name = node["extname"].as_str().unwrap_or_default();
                let run = self.tokens.name_after(&["extension"], self.start, self.end);
                self.define(ObjKind::Extension, Key::new(Space::Extension, "", name), run, |_| {});
            }
            "AlterTableStmt" => self.alter_table(),
            "DoStmt" => {
                for opt in node["args"].as_array().into_iter().flatten() {
                    let d = &opt["DefElem"];
                    if d["defname"] == "as" {
                        self.plpgsql_body(d, None, None);
                    }
                }
            }
            "VariableSetStmt" => {
                if node["name"] == "search_path" {
                    let path = search_path_of(node);
                    if !path.is_empty() {
                        self.file.search_path = path;
                    }
                }
            }
            "DropStmt" => self.named_objects(&node["objects"], space_of_object_type(node["removeType"].as_str()), RefKind::Drop),
            "GrantStmt" => {
                let space = space_of_object_type(node["objtype"].as_str());
                if space == Some(Space::Relation) {
                    self.walk(&node["objects"], RefKind::Grant, None);
                } else {
                    self.named_objects(&node["objects"], space, RefKind::Grant);
                }
            }
            "CommentStmt" => {
                let object = Json::Array(vec![node["object"].clone()]);
                self.named_objects(&object, space_of_object_type(node["objtype"].as_str()), RefKind::Comment);
            }
            "AlterFunctionStmt" => {
                let object = Json::Array(vec![node["func"].clone()]);
                self.named_objects(&object, Some(Space::Routine), RefKind::Alter);
            }
            "AlterOwnerStmt" | "AlterObjectSchemaStmt" | "RenameStmt" => {
                if !node["relation"].is_null() {
                    self.walk(&node["relation"], RefKind::Alter, None);
                }
                if !node["object"].is_null() {
                    let object = Json::Array(vec![node["object"].clone()]);
                    let t = node["objectType"].as_str().or(node["renameType"].as_str());
                    self.named_objects(&object, space_of_object_type(t), RefKind::Alter);
                }
            }
            ty => {
                let wrapped = serde_json::json!({ ty: node });
                self.walk(&wrapped, RefKind::Read, None);
            }
        }
    }

    // ------------------------------------------------------------------ definitions

    fn run_at(&self, loc: &Json) -> Option<Run> {
        self.tokens.qualified_starting_at(location(loc)?)
    }

    fn define(&mut self, kind: ObjKind, key: Key, run: Option<Run>, fill: impl FnOnce(&mut Definition)) {
        let name = run.map_or(self.start..self.start, |r| r.start..r.end);
        let mut d = Definition {
            key,
            kind,
            name: self.base + name.start..self.base + name.end,
            statement: self.base + self.start..self.base + self.end,
            is_alter: false,
            columns: Vec::new(),
            dropped_columns: Vec::new(),
            params: Vec::new(),
            returns: None,
            language: None,
            changeset: self.file.changeset.map(|c| format!("{}:{}", c.author, c.id)),
            comment: self.file.changeset.map(|c| c.comment.clone()).unwrap_or_default(),
            table: None,
        };
        fill(&mut d);
        self.file.out.definitions.push(d);
    }

    fn create_table(&mut self) {
        let node = self.node;
        let rv = &node["relation"];
        let key = Key::new(Space::Relation, &self.schema(&rv["schemaname"]), &self.r(rv["relname"].as_str().unwrap_or_default()));
        let run = self.run_at(&rv["location"]);
        let columns = self.columns(&node["tableElts"]);
        self.define(ObjKind::Table, key.clone(), run, |d| d.columns = columns);
        self.walk_children(node, Some(&key), &["relation"]);
    }

    fn create_view(&mut self) {
        let node = self.node;
        let rv = &node["view"];
        let key = Key::new(Space::Relation, &self.schema(&rv["schemaname"]), &self.r(rv["relname"].as_str().unwrap_or_default()));
        let aliases = names(&node["aliases"]);
        let columns = if aliases.is_empty() { select_columns(&node["query"]) } else { aliases.into_iter().map(|a| Column { name: a, ty: String::new(), not_null: false }).collect() };
        let run = self.run_at(&rv["location"]);
        self.define(ObjKind::View, key.clone(), run, |d| d.columns = columns);
        self.walk(&node["query"], RefKind::Read, Some(&key));
    }

    fn create_table_as(&mut self) {
        let node = self.node;
        let rv = &node["into"]["rel"];
        let key = Key::new(Space::Relation, &self.schema(&rv["schemaname"]), &self.r(rv["relname"].as_str().unwrap_or_default()));
        let kind = if node["objtype"] == "OBJECT_MATVIEW" { ObjKind::MaterializedView } else { ObjKind::Table };
        let columns = select_columns(&node["query"]);
        let run = self.run_at(&rv["location"]);
        self.define(kind, key.clone(), run, |d| d.columns = columns);
        self.walk(&node["query"], RefKind::Read, Some(&key));
    }

    fn create_index(&mut self) {
        let node = self.node;
        let rv = &node["relation"];
        let table = QName { schema: rv["schemaname"].as_str().map(|s| self.r(s)), name: self.r(rv["relname"].as_str().unwrap_or_default()) };
        let mut from = Key::new(Space::Relation, table.schema.as_deref().unwrap_or("public"), &table.name);
        if let Some(idx) = node["idxname"].as_str() {
            let key = Key::new(Space::Relation, table.schema.as_deref().unwrap_or("public"), &self.r(idx));
            let run = self.tokens.name_after(&["index"], self.start, self.end);
            let t = table.clone();
            self.define(ObjKind::Index, key.clone(), run, |d| d.table = Some(t));
            from = key;
        }
        self.walk(rv, RefKind::IndexOn, Some(&from));
        self.walk_children(node, Some(&from), &["relation"]);
    }

    fn create_trigger(&mut self) {
        let node = self.node;
        let rv = &node["relation"];
        let table = QName { schema: rv["schemaname"].as_str().map(|s| self.r(s)), name: self.r(rv["relname"].as_str().unwrap_or_default()) };
        let mut key = Key::new(Space::Trigger, table.schema.as_deref().unwrap_or("public"), &self.r(node["trigname"].as_str().unwrap_or_default()));
        key.signature = Some(table.name.clone());
        let run = self.tokens.name_after(&["trigger"], self.start, self.end);
        let t = table.clone();
        self.define(ObjKind::Trigger, key.clone(), run, |d| d.table = Some(t));
        self.walk(rv, RefKind::Trigger, Some(&key));
        let function = names(&node["funcname"]);
        let execute = self.keyword_index(&["execute"], None).map_or(self.start, |i| self.tokens.toks.get(i).map_or(self.start, |t| t.start));
        if let Some(run) = self.tokens.find(&function, execute, self.end) {
            let name = self.qname(&function);
            self.add_ref(name, Space::Routine, RefKind::Trigger, run.start, run.end, Some(&key), Some(0));
        }
        self.walk_children(node, Some(&key), &["relation", "funcname"]);
    }

    fn alter_table(&mut self) {
        let node = self.node;
        let rv = &node["relation"];
        let key = Key::new(Space::Relation, &self.schema(&rv["schemaname"]), &self.r(rv["relname"].as_str().unwrap_or_default()));
        if node["objtype"].as_str().is_some_and(|t| t != "OBJECT_TABLE") {
            self.walk(rv, RefKind::Alter, None);
            self.walk_children(node, Some(&key), &["relation"]);
            return;
        }
        let mut added = Vec::new();
        let mut dropped = Vec::new();
        for c in node["cmds"].as_array().into_iter().flatten() {
            let cmd = &c["AlterTableCmd"];
            match cmd["subtype"].as_str() {
                Some("AT_AddColumn") => added.extend(self.columns(&Json::Array(vec![cmd["def"].clone()]))),
                Some("AT_DropColumn") => dropped.extend(cmd["name"].as_str().map(str::to_string)),
                _ => {}
            }
        }
        let run = self.run_at(&rv["location"]);
        self.define(ObjKind::Table, key.clone(), run.clone(), |d| {
            d.is_alter = true;
            d.columns = added;
            d.dropped_columns = dropped;
        });
        // ALTER TABLE x is also a use of x.
        if let Some(run) = run {
            let name = QName { schema: rv["schemaname"].as_str().map(|s| self.r(s)), name: key.name.clone() };
            self.add_ref(name, Space::Relation, RefKind::Alter, run.start, run.end, None, None);
        }
        self.walk_children(node, Some(&key), &["relation"]);
    }

    fn create_routine(&mut self) {
        let node = self.node;
        let function = names(&node["funcname"]);
        let is_procedure = node["is_procedure"].as_bool() == Some(true);
        let schema = if function.len() > 1 { self.r(&function[0]) } else { "public".into() };
        let name_run = self.tokens.name_after(&["function", "procedure"], self.start, self.end);
        let mut cursor = name_run.as_ref().map_or_else(|| self.tokens.index_at(self.start), |r| r.last + 2);
        let mut params = Vec::new();
        let mut inputs = Vec::new();
        for p in node["parameters"].as_array().into_iter().flatten() {
            let fp = &p["FunctionParameter"];
            let mode = match fp["mode"].as_str() {
                Some("FUNC_PARAM_OUT") => "out",
                Some("FUNC_PARAM_INOUT") => "inout",
                Some("FUNC_PARAM_VARIADIC") => "variadic",
                Some("FUNC_PARAM_TABLE") => "table",
                _ => "in",
            };
            let type_node = &fp["argType"];
            let (ty, after) = match location(&type_node["location"]) {
                Some(at) => self.text_until(self.tokens.index_at(at), &["default"]),
                None => (String::new(), cursor),
            };
            cursor = after;
            params.push(Param { name: fp["name"].as_str().map(str::to_string), mode, ty: self.r(&ty), has_default: !fp["defexpr"].is_null() });
            if mode != "out" && mode != "table" {
                inputs.push(type_key(type_node));
            }
        }
        let mut key = Key::new(Space::Routine, &schema, &self.r(function.last().map_or("", String::as_str)));
        key.signature = Some(inputs.join(","));
        let mut language = None;
        let mut body = None;
        let mut path = self.file.search_path.clone();
        for o in node["options"].as_array().into_iter().flatten() {
            let d = &o["DefElem"];
            match d["defname"].as_str() {
                Some("language") => language = d["arg"]["String"]["sval"].as_str().map(str::to_string),
                Some("as") => body = Some(d),
                Some("set") if d["arg"]["VariableSetStmt"]["name"] == "search_path" => path = search_path_of(&d["arg"]["VariableSetStmt"]),
                _ => {}
            }
        }
        let returns = if is_procedure {
            None
        } else {
            self.keyword_index(&["returns"], Some(cursor)).map(|i| self.r(&self.text_until(i, RETURNS_STOPS).0)).filter(|r| !r.is_empty())
        };
        let lang = language.clone();
        self.define(if is_procedure { ObjKind::Procedure } else { ObjKind::Function }, key.clone(), name_run.clone(), |d| {
            d.params = params;
            d.returns = returns;
            d.language = lang;
        });
        self.walk(&node["parameters"], RefKind::Type, Some(&key));
        self.walk(&node["returnType"], RefKind::Type, Some(&key));
        if !node["sql_body"].is_null() {
            self.walk(&node["sql_body"], RefKind::Read, Some(&key));
        }
        if let Some(body) = body {
            path.retain(|s| s != "pg_temp");
            match language.as_deref() {
                Some("plpgsql") => self.plpgsql_body(body, Some(&key), Some(path)),
                Some("sql") => self.sql_body(body, &key, path),
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------------ bodies

    /// The dollar-quoted body of a DefElem `as`: its byte range in the parsed text.
    fn body_text(&self, def: &Json) -> Option<std::ops::Range<usize>> {
        let at = location(&def["arg_location"]).or_else(|| location(&def["location"]))?;
        let mut i = self.tokens.index_at(at);
        // From the AS keyword: the first string after it.
        while self.tokens.toks.get(i).is_some_and(|t| !t.is_string() && t.start < self.end) {
            i += 1;
        }
        let t = self.tokens.toks.get(i)?;
        let raw = self.tokens.text_of(t);
        if !raw.starts_with('$') {
            return None; // quoted bodies: escapes would shift the offsets
        }
        let close = raw[1..].find('$')? + 2;
        if raw.len() < close * 2 {
            return None;
        }
        Some(t.start + close..t.end - close)
    }

    fn plpgsql_body(&mut self, def: &Json, from: Option<&Key>, path: Option<Vec<String>>) {
        let Some(range) = self.body_text(def) else { return };
        let sql = self.sql;
        let (start, body) = (range.start, &sql[range]);
        let path = path.unwrap_or_else(|| self.file.search_path.clone());
        let refs = scan_plpgsql_body(body, self.base + start, from, &path, false);
        self.file.out.references.extend(refs);
        self.check_plpgsql(start, body, from.is_none());
    }

    fn sql_body(&mut self, def: &Json, from: &Key, path: Vec<String>) {
        let Some(range) = self.body_text(def) else { return };
        let sql = self.sql;
        let (start, body) = (range.start, &sql[range]);
        match ffi::parse_json(body) {
            Ok(tree) => {
                let tokens = Tokens::new(body);
                let mut walker = Walker { base: self.base + start, tokens: &tokens, search_path: &path, properties: self.file.properties, out: &mut self.file.out.references };
                for s in tree["stmts"].as_array().into_iter().flatten() {
                    walker.walk(&s["stmt"], RefKind::Read, Some(from));
                }
            }
            Err(e) => self.file.syntax_error(body, self.base + start, &e),
        }
    }

    /// Compiles a PL/pgSQL body. libpg_query cannot look up types of other schemas in the
    /// parameters and DECLARE, so those become built-in types of the same width first.
    fn check_plpgsql(&mut self, body_start: usize, body: &str, is_do: bool) {
        let (check, body_at) = if is_do {
            let head = "CREATE FUNCTION pg_temp.__do() RETURNS void LANGUAGE plpgsql AS $__body$";
            (format!("{head}{body}$__body$;"), head.len())
        } else {
            let statement = &self.sql[self.start..self.end];
            (neutralize_types(statement, body_start - self.start, body.len()), body_start - self.start)
        };
        let Some(error) = ffi::plpgsql_error(&check) else { return };
        let local = locate_plpgsql_error(&error, &check, body_at, body.len());
        let at = body_start + local;
        let end = error_end(self.sql, at);
        self.file.out.problems.push(Problem { severity: Severity::Error, message: error.message, span: self.base + at..self.base + end, code: "plpgsql" });
    }

    // ------------------------------------------------------------------ references

    fn walk(&mut self, node: &Json, kind: RefKind, from: Option<&Key>) {
        let path = self.file.search_path.clone();
        let mut walker = Walker { base: self.base, tokens: self.tokens, search_path: &path, properties: self.file.properties, out: &mut self.file.out.references };
        walker.walk(node, kind, from);
    }

    fn walk_children(&mut self, node: &Json, from: Option<&Key>, skip: &[&str]) {
        for (k, v) in node.as_object().into_iter().flatten() {
            if !skip.contains(&k.as_str()) {
                self.walk(v, RefKind::Read, from);
            }
        }
    }

    /// DROP / GRANT / COMMENT ON objects: names without locations, found in the text in order.
    fn named_objects(&mut self, objects: &Json, space: Option<Space>, kind: RefKind) {
        let Some(space) = space else { return };
        let mut cursor = self.start;
        for o in objects.as_array().into_iter().flatten() {
            let mut args = None;
            let parts = if let Some(owa) = o.get("ObjectWithArgs") {
                if owa["args_unspecified"].as_bool() != Some(true) {
                    args = Some(owa["objargs"].as_array().map_or(0, Vec::len));
                }
                let objargs = owa["objargs"].clone();
                self.walk(&objargs, RefKind::Type, None);
                names(&owa["objname"])
            } else if let Some(list) = o.get("List") {
                names(&list["items"])
            } else if let Some(s) = o["String"]["sval"].as_str() {
                vec![s.to_string()]
            } else if let Some(tn) = o.get("TypeName") {
                names(&tn["names"])
            } else {
                continue;
            };
            if parts.is_empty() {
                continue;
            }
            let Some(run) = self.tokens.find(&parts, cursor, self.end) else { continue };
            cursor = run.end;
            let name = self.qname(&parts);
            self.add_ref(name, space, kind, run.start, run.end, None, args);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add_ref(&mut self, name: QName, space: Space, kind: RefKind, start: usize, end: usize, from: Option<&Key>, args: Option<usize>) {
        if name.schema.as_deref().is_some_and(|s| BUILTIN_SCHEMAS.contains(&s)) {
            return;
        }
        self.file.out.references.push(Reference {
            name,
            space,
            kind,
            span: self.base + start..self.base + end,
            from: from.cloned(),
            args,
            search_path: self.file.search_path.clone(),
            dynamic: false,
        });
    }

    // ------------------------------------------------------------------ helpers

    /// Index of the token after the first of `keywords` (from token `from`, else the statement's
    /// start) within the statement.
    fn keyword_index(&self, keywords: &[&str], from: Option<usize>) -> Option<usize> {
        let mut i = from.unwrap_or_else(|| self.tokens.index_at(self.start));
        while i < self.tokens.toks.len() && self.tokens.toks[i].start < self.end {
            if self.tokens.toks[i].is_keyword() && keywords.contains(&self.tokens.word(i).as_str()) {
                return Some(i + 1);
            }
            i += 1;
        }
        None
    }

    /// Source text from token `i` to the first token at depth 0 that is `,` `)` `;` or one of the
    /// keywords `stops`, with whitespace collapsed (types as written: `numeric(12, 2)`).
    fn text_until(&self, i: usize, stops: &[&str]) -> (String, usize) {
        let t = self.tokens;
        let mut depth = 0i32;
        let mut j = i;
        while j < t.toks.len() && t.toks[j].start < self.end {
            let s = t.text_of(&t.toks[j]);
            if depth == 0 && (s == "," || s == ")" || s == ";" || (t.toks[j].is_keyword() && stops.contains(&t.word(j).as_str()))) {
                break;
            }
            match s {
                "(" | "[" => depth += 1,
                ")" | "]" => depth -= 1,
                _ => {}
            }
            j += 1;
        }
        if j == i {
            return (String::new(), j);
        }
        let text = &self.sql[t.toks[i].start..t.toks[j - 1].end];
        (text.split_whitespace().collect::<Vec<_>>().join(" "), j)
    }

    fn type_text(&self, type_node: &Json, stops: &[&str]) -> Option<String> {
        let at = location(&type_node["location"])?;
        Some(self.r(&self.text_until(self.tokens.index_at(at), stops).0)).filter(|t| !t.is_empty())
    }

    fn columns(&self, elements: &Json) -> Vec<Column> {
        let mut out = Vec::new();
        for e in elements.as_array().into_iter().flatten() {
            let def = &e["ColumnDef"];
            let Some(name) = def["colname"].as_str() else { continue };
            let ty = self.type_text(&def["typeName"], TYPE_STOPS).unwrap_or_default();
            let not_null = def["is_not_null"].as_bool() == Some(true)
                || def["constraints"].as_array().into_iter().flatten().any(|c| matches!(c["Constraint"]["contype"].as_str(), Some("CONSTR_NOTNULL" | "CONSTR_PRIMARY")));
            out.push(Column { name: self.r(name), ty, not_null });
        }
        out
    }
}

/// Normalized type of a TypeName for a routine's identity: `uuid`, `int8`, `s.line_input[]`.
fn type_key(type_name: &Json) -> String {
    let n = names(&type_name["names"]);
    let base = if n.len() == 2 && n[0] == "pg_catalog" { n[1].clone() } else { n.join(".") };
    base + &"[]".repeat(type_name["arrayBounds"].as_array().map_or(0, Vec::len))
}

fn select_columns(query: &Json) -> Vec<Column> {
    let select = &query["SelectStmt"];
    if select.is_null() {
        return Vec::new();
    }
    if select["targetList"].is_null() && !select["larg"].is_null() {
        return select_columns(&serde_json::json!({ "SelectStmt": select["larg"] }));
    }
    let mut out = Vec::new();
    for t in select["targetList"].as_array().into_iter().flatten() {
        let rt = &t["ResTarget"];
        let mut name = rt["name"].as_str().map(str::to_string);
        if name.is_none()
            && let Some(fields) = rt["val"]["ColumnRef"]["fields"].as_array()
        {
            name = Some(fields.last().and_then(|f| f["String"]["sval"].as_str()).unwrap_or("*").to_string());
        }
        if name.is_none() && !rt["val"]["FuncCall"].is_null() {
            name = names(&rt["val"]["FuncCall"]["funcname"]).pop();
        }
        out.push(Column { name: name.unwrap_or_else(|| "?column?".into()), ty: String::new(), not_null: false });
    }
    out
}

fn search_path_of(set: &Json) -> Vec<String> {
    set["args"].as_array().into_iter().flatten().filter_map(|a| a["A_Const"]["sval"]["sval"].as_str().map(str::to_string)).collect()
}

/// Replaces schema-qualified types in the parameters and the DECLARE sections of `statement`
/// with built-in types of the same width, so libpg_query compiles it and offsets stay.
fn neutralize_types(statement: &str, body_start: usize, body_len: usize) -> String {
    let mut bytes = statement.as_bytes().to_vec();
    let qualified = |r: &Run| r.parts.len() >= 2 && !BUILTIN_SCHEMAS.contains(&r.parts[0].as_str());
    let put = |bytes: &mut Vec<u8>, at: usize, width: usize, word: &str| {
        if width >= word.len() {
            let padded = format!("{word:width$}");
            bytes[at..at + width].copy_from_slice(padded.as_bytes());
        }
    };
    let t = Tokens::new(statement);
    fn after<'x>(t: &Tokens<'x>, r: &Run) -> &'x str {
        t.toks.get(r.last + 1).map_or("", |x| t.text_of(x))
    }
    if let Some(name) = t.name_after(&["function", "procedure"], 0, statement.len())
        && after(&t, &name) == "("
    {
        let close = t.matching_paren(name.last + 1);
        let mut i = name.last + 2;
        while i < close {
            if let Some(run) = t.qualified_at(i) {
                if qualified(&run) {
                    let word = if after(&t, &run) == "[" || run.end - run.start < 6 { "int" } else { "record" };
                    put(&mut bytes, run.start, run.end - run.start, word);
                }
                i = run.last;
            }
            i += 1;
        }
    }
    let body = &statement[body_start..(body_start + body_len).min(statement.len())];
    let b = Tokens::new(body);
    let mut in_declare = false;
    let mut i = 0;
    while i < b.toks.len() {
        match b.word(i).as_str() {
            "declare" => in_declare = true,
            "begin" => in_declare = false,
            _ => {}
        }
        if in_declare && let Some(run) = b.qualified_at(i) {
            let next = after(&b, &run);
            if qualified(&run) && next == "%" && run.last + 2 < b.toks.len() {
                // s.t%ROWTYPE → record, s.t.c%TYPE → text: there is no relation to look up.
                let replacement = if b.word(run.last + 2) == "rowtype" { "record" } else { "text" };
                put(&mut bytes, body_start + run.start, b.toks[run.last + 2].end - run.start, replacement);
                i = run.last + 3;
                continue;
            }
            if qualified(&run) && next != "%" && next != "(" {
                let word = if next == "[" || run.end - run.start < 6 { "int" } else { "record" };
                put(&mut bytes, body_start + run.start, run.end - run.start, word);
            }
            i = run.last + 1;
            continue;
        }
        i += 1;
    }
    String::from_utf8(bytes).unwrap_or_else(|_| statement.to_string())
}

/// libpg_query gives no position for PL/pgSQL errors: "near line N" of the context and the
/// token of `at or near "X"` find it in the body (best effort).
fn locate_plpgsql_error(error: &ParseError, check: &str, body_at: usize, body_len: usize) -> usize {
    let body = &check[body_at.min(check.len())..(body_at + body_len).min(check.len())];
    let mut from = 0;
    if let Some(n) = error.context.split("line ").nth(1).and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next()).and_then(|s| s.parse::<usize>().ok()) {
        for _ in 1..n {
            match body[from..].find('\n') {
                Some(nl) => from += nl + 1,
                None => break,
            }
        }
    }
    let quoted = error.message.split('"').nth(1).filter(|_| error.message.contains('"'));
    if let Some(word) = quoted.filter(|w| !w.is_empty()) {
        if let Some(at) = body[from..].find(word) {
            return from + at;
        }
        if let Some(at) = body.find(word) {
            return at;
        }
    }
    from.min(body.len())
}

/// Walks a JSON parse tree and records references with what they do.
pub(crate) struct Walker<'a> {
    /// File offset of the parsed text's first byte.
    pub base: usize,
    pub tokens: &'a Tokens<'a>,
    pub search_path: &'a [String],
    pub properties: &'a [String],
    pub out: &'a mut Vec<Reference>,
}

impl Walker<'_> {
    pub fn walk(&mut self, node: &Json, kind: RefKind, from: Option<&Key>) {
        match node {
            Json::Array(items) => {
                for n in items {
                    self.walk(n, kind, from);
                }
            }
            Json::Object(map) => {
                // Typed fields hold nodes without their wrapper ("relation": {"relname": …}).
                if map.contains_key("relname") && map.contains_key("relpersistence") {
                    return self.range_var(node, kind, from);
                }
                if map.contains_key("names") && map.contains_key("typemod") {
                    return self.type_name(node, from);
                }
                if map.contains_key("funcname") && map.contains_key("funcformat") {
                    return self.func_call(node, from);
                }
                for (key, value) in map {
                    match key.as_str() {
                        "RangeVar" => self.range_var(value, kind, from),
                        "FuncCall" => self.func_call(value, from),
                        "TypeName" => self.type_name(value, from),
                        "InsertStmt" | "UpdateStmt" | "DeleteStmt" | "MergeStmt" => {
                            for (k, v) in value.as_object().into_iter().flatten() {
                                self.walk(v, if k == "relation" { RefKind::Write } else { RefKind::Read }, from);
                            }
                        }
                        "TruncateStmt" => self.walk(&value["relations"], RefKind::Write, from),
                        "Constraint" => {
                            for (k, v) in value.as_object().into_iter().flatten() {
                                self.walk(v, if k == "pktable" { RefKind::ForeignKey } else { RefKind::Read }, from);
                            }
                        }
                        "ColumnDef" | "FunctionParameter" => {
                            for (k, v) in value.as_object().into_iter().flatten() {
                                self.walk(v, if k == "typeName" || k == "argType" { RefKind::Type } else { RefKind::Read }, from);
                            }
                        }
                        "TypeCast" => {
                            self.walk(&value["arg"], RefKind::Read, from);
                            self.walk(&value["typeName"], RefKind::Type, from);
                        }
                        _ if value.is_object() || value.is_array() => self.walk(value, kind, from),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn func_call(&mut self, fc: &Json, from: Option<&Key>) {
        let args = fc["args"].as_array().map_or(0, Vec::len);
        self.named(&fc["funcname"], Space::Routine, RefKind::Call, &fc["location"], from, Some(args));
        for k in ["args", "agg_filter", "over", "agg_order"] {
            self.walk(&fc[k], RefKind::Read, from);
        }
    }

    fn type_name(&mut self, tn: &Json, from: Option<&Key>) {
        self.named(&tn["names"], Space::Type, RefKind::Type, &tn["location"], from, None);
    }

    fn range_var(&mut self, rv: &Json, kind: RefKind, from: Option<&Key>) {
        let Some(at) = location(&rv["location"]) else { return };
        let name = rv["relname"].as_str().unwrap_or_default();
        let end = self.tokens.qualified_starting_at(at).map_or(at + name.len(), |r| r.end);
        let kind = if matches!(kind, RefKind::Type | RefKind::Call) { RefKind::Read } else { kind };
        let q = QName { schema: rv["schemaname"].as_str().map(|s| restore(s, self.properties)), name: restore(name, self.properties) };
        self.add(q, Space::Relation, kind, at, end, from, None);
    }

    fn named(&mut self, list: &Json, space: Space, kind: RefKind, loc: &Json, from: Option<&Key>, args: Option<usize>) {
        let parts = names(list);
        let Some(at) = location(loc) else { return };
        // Built-in types come back as pg_catalog.int8 at the keyword: not references.
        if parts.is_empty() || (parts.len() >= 2 && BUILTIN_SCHEMAS.contains(&parts[0].as_str())) {
            return;
        }
        let Some(run) = self.tokens.qualified_starting_at(at) else { return };
        let q = match parts.as_slice() {
            [.., schema, name] => QName { schema: Some(restore(schema, self.properties)), name: restore(name, self.properties) },
            [name] => QName { schema: None, name: restore(name, self.properties) },
            [] => return,
        };
        self.add(q, space, kind, at, run.end, from, args);
    }

    #[allow(clippy::too_many_arguments)]
    fn add(&mut self, name: QName, space: Space, kind: RefKind, start: usize, end: usize, from: Option<&Key>, args: Option<usize>) {
        if name.schema.as_deref().is_some_and(|s| BUILTIN_SCHEMAS.contains(&s)) {
            return;
        }
        self.out.push(Reference { name, space, kind, span: self.base + start..self.base + end, from: from.cloned(), args, search_path: self.search_path.to_vec(), dynamic: false });
    }
}
