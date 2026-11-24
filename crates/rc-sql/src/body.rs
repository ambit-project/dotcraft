//! References in a PL/pgSQL body from its tokens. libpg_query's PL/pgSQL tree has no positions
//! of expressions, so the body is read token by token with the clause each name is in. Names in
//! the constant text of dynamic SQL (`EXECUTE '…'`, `format('…')`) count too.
//!
//! A port of PG Studio's `analysis/body_scan.dart`.

use crate::analyze::BUILTIN_SCHEMAS;
use crate::model::{Key, QName, RefKind, Reference, Space};
use crate::tokens::Tokens;

const KEYWORDS: &[&str] = &[
    "select", "insert", "update", "delete", "merge", "truncate", "perform", "call", "from", "join", "into", "table", "only", "using", "declare", "begin", "then", "else",
    "elsif", "loop", "end", "for", "return", "returning", "set", "where", "values", "on", "and", "or", "not", "in", "is", "as", "with", "exception", "when", "raise", "if",
    "case", "lateral", "exists", "distinct", "group", "order", "by", "having", "limit", "offset", "union", "except", "intersect", "execute", "foreach", "while", "array",
    "get", "diagnostics", "null", "true", "false", "constant", "default", "strict", "left", "right", "inner", "outer", "full", "cross", "natural", "any", "all", "some",
];
const VERBS: &[&str] = &["select", "insert", "update", "delete", "merge", "truncate", "perform", "call", "return"];
const STATEMENT_BREAKS: &[&str] = &[";", "begin", "then", "else", "elsif", "loop", "declare"];
const RELATION_CLAUSES: &[&str] = &["from", "join", "into", "update", "table", "only", "using", "truncate"];

/// References of `body`, whose first byte is at `offset` of the file.
pub fn scan_plpgsql_body(body: &str, offset: usize, from: Option<&Key>, search_path: &[String], dynamic: bool) -> Vec<Reference> {
    let t = Tokens::new(body);
    let mut out = Vec::new();
    let add = |out: &mut Vec<Reference>, name: QName, space: Space, kind: RefKind, start: usize, end: usize, args: Option<usize>| {
        if name.schema.as_deref().is_some_and(|s| BUILTIN_SCHEMAS.contains(&s)) {
            return;
        }
        out.push(Reference { name, space, kind, span: offset + start..offset + end, from: from.cloned(), args, search_path: search_path.to_vec(), dynamic });
    };
    let mut scanned = std::collections::HashSet::new();
    let mut scan_literal = |out: &mut Vec<Reference>, k: usize| {
        if dynamic || !scanned.insert(k) {
            return;
        }
        let tok = &t.toks[k];
        if let Some((sql, at)) = literal_sql(t.text_of(tok)) {
            out.extend(scan_plpgsql_body(&sql, offset + tok.start + at, from, search_path, true));
        }
    };
    let mut verb: Option<String> = None;
    let mut prev: Option<String> = None;
    let mut in_declare = false;
    let mut i = 0;
    while i < t.toks.len() {
        let tok = t.toks[i];
        let word = t.word(i);

        if !dynamic && word == "execute" {
            // EXECUTE 'text' || … / EXECUTE format('text', …): every literal up to INTO, USING or the end.
            let mut depth = 0i32;
            for k in i + 1..t.toks.len() {
                let s = t.text_of(&t.toks[k]);
                let lower = s.to_ascii_lowercase();
                if s == "(" {
                    depth += 1;
                }
                if s == ")" {
                    depth -= 1;
                }
                if s == ";" || depth < 0 || (depth == 0 && (lower == "into" || lower == "using")) {
                    break;
                }
                if t.toks[k].is_string() && (depth == 0 || is_format_arg(&t, k)) {
                    scan_literal(&mut out, k);
                }
            }
        } else if !dynamic && word == "format" && t.is_symbol(i + 1, "(") && t.toks.get(i + 2).is_some_and(|x| x.is_string()) {
            scan_literal(&mut out, i + 2);
        }

        if KEYWORDS.contains(&word.as_str()) || !tok.is_name() {
            if STATEMENT_BREAKS.contains(&word.as_str()) {
                verb = None;
            }
            match word.as_str() {
                "declare" => in_declare = true,
                "begin" => in_declare = false,
                _ => {}
            }
            let after_for = prev.as_deref() == Some("for");
            if VERBS.contains(&word.as_str()) && !(word == "update" && after_for) && verb.is_none() {
                verb = Some(word.clone());
            }
            if matches!(word.as_str(), "insert" | "delete" | "merge" | "truncate") || (word == "update" && !after_for) {
                verb = Some(word.clone());
            }
            prev = Some(word);
            i += 1;
            continue;
        }
        if i > 0 && t.is_symbol(i - 1, ".") {
            prev = Some(word);
            i += 1;
            continue;
        }

        let Some(run) = t.qualified_at(i) else {
            i += 1;
            continue;
        };
        let next = t.toks.get(run.last + 1).map_or("", |x| t.text_of(x));
        let parts = &run.parts;
        let first_part_end = if parts.len() >= 2 { t.toks[run.first + 2].end } else { t.toks[run.first].end };
        let qname = |parts: &[String]| if parts.len() >= 2 { QName { schema: Some(parts[0].clone()), name: parts[1].clone() } } else { QName { schema: None, name: parts[0].clone() } };

        // `schema.%I` in dynamic SQL: the object's name is a parameter.
        if dynamic && next == "." {
            prev = None;
            i = run.last + 1;
            continue;
        }
        let verb_is = |v: &str| verb.as_deref() == Some(v);
        let prev_is = |p: &str| prev.as_deref() == Some(p);
        let insert_target = prev_is("into") && (verb_is("insert") || verb_is("merge"));
        if next == "(" && parts.len() <= 2 && !insert_target {
            let close = t.matching_paren(run.last + 1);
            add(&mut out, qname(parts), Space::Routine, RefKind::Call, run.start, run.end, Some(t.arg_count(run.last + 1, close)));
        } else if next == "%" && run.last + 2 < t.toks.len() {
            let what = t.word(run.last + 2);
            if what == "rowtype" && parts.len() >= 2 {
                add(&mut out, qname(parts), Space::Type, RefKind::Type, run.start, first_part_end, None);
            } else if what == "type" && parts.len() == 3 {
                add(&mut out, qname(parts), Space::Relation, RefKind::Type, run.start, first_part_end, None);
            } else if what == "type" && parts.len() == 2 {
                add(&mut out, QName { schema: None, name: parts[0].clone() }, Space::Relation, RefKind::Type, run.start, t.toks[run.first].end, None);
            }
        } else if parts.len() >= 2 && (prev_is("::") || in_declare) {
            add(&mut out, qname(parts), Space::Type, RefKind::Type, run.start, first_part_end, None);
        } else if prev.as_deref().is_some_and(|p| RELATION_CLAUSES.contains(&p)) {
            let write = (prev_is("into") && (verb_is("insert") || verb_is("merge"))) || (prev_is("update") && verb_is("update")) || (prev_is("from") && verb_is("delete")) || verb_is("truncate");
            // SELECT … INTO target: a variable, not a relation.
            let select_into = prev_is("into") && !verb_is("insert") && !verb_is("merge");
            if !select_into {
                add(&mut out, qname(parts), Space::Relation, if write { RefKind::Write } else { RefKind::Read }, run.start, first_part_end, None);
            }
        } else if parts.len() >= 2 {
            // alias.column, record.field or schema.object: only a known schema makes it one.
            add(&mut out, qname(parts), Space::Relation, RefKind::Read, run.start, first_part_end, None);
        }
        prev = if parts.len() == 1 { Some(word) } else { None };
        i = run.last + 1;
    }
    out
}

/// The literal at token `k` is the first argument of `format(`.
fn is_format_arg(t: &Tokens, k: usize) -> bool {
    k >= 2 && t.is_symbol(k - 1, "(") && t.word(k - 2) == "format"
}

/// The SQL inside a string literal, as long as its text (offsets map 1:1): `''` becomes `' `
/// and format placeholders (`%I`, `%1$L`, `%s`) become parameters (`$1`). Returns the SQL and
/// where it starts in the literal; None for E'…' and U&'…'.
fn literal_sql(literal: &str) -> Option<(String, usize)> {
    let (content, at) = if literal.len() >= 2 && literal.starts_with('\'') && literal.ends_with('\'') {
        (literal[1..literal.len() - 1].replace("''", "' "), 1)
    } else {
        let close = literal.strip_prefix('$')?.find('$')? + 2;
        let tag = &literal[..close];
        if literal.len() < close * 2 || !literal.ends_with(tag) {
            return None;
        }
        (literal[close..literal.len() - close].to_string(), close)
    };
    Some((replace_format_specs(&content), at))
}

/// `%I` `%L` `%s` `%1$I` `%-10s` `%*s` → `$1` padded to the same width; `%%` → two spaces.
fn replace_format_specs(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if bytes.get(i + 1) == Some(&b'%') {
                out.push_str("  ");
                i += 2;
                continue;
            }
            let mut j = i + 1;
            // %n$
            let digits = bytes[j..].iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 0 && bytes.get(j + digits) == Some(&b'$') {
                j += digits + 1;
            }
            if bytes.get(j) == Some(&b'-') {
                j += 1;
            }
            if bytes.get(j) == Some(&b'*') {
                j += 1;
                let d = bytes[j..].iter().take_while(|b| b.is_ascii_digit()).count();
                if d > 0 && bytes.get(j + d) == Some(&b'$') {
                    j += d + 1;
                }
            } else {
                j += bytes[j..].iter().take_while(|b| b.is_ascii_digit()).count();
            }
            if matches!(bytes.get(j), Some(b's' | b'I' | b'L')) {
                let width = j + 1 - i;
                out.push_str(&format!("{:width$}", "$1"));
                i = j + 1;
                continue;
            }
        }
        let c = text[i..].chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(body: &str) -> Vec<(String, Space, RefKind, String)> {
        scan_plpgsql_body(body, 0, None, &["public".into()], false).into_iter().map(|r| (r.name.to_string(), r.space, r.kind, body[r.span].to_string())).collect()
    }

    #[test]
    fn statements_read_write_and_call() {
        let r = refs(
            "declare v orders.t%rowtype; n crm.money;\nbegin\n  insert into orders.t (id) select id from crm.c join crm.d on true;\n  update orders.t set x = 1;\n  delete from crm.c;\n  perform orders_app.notify(1, 2);\n  select a into v from orders.t;\nend",
        );
        let has = |name: &str, kind: RefKind| r.iter().any(|x| x.0 == name && x.2 == kind);
        assert!(has("orders.t", RefKind::Type), "{r:?}");
        assert!(has("crm.money", RefKind::Type));
        assert!(has("orders.t", RefKind::Write));
        assert!(has("crm.c", RefKind::Read));
        assert!(has("crm.d", RefKind::Read));
        assert!(has("crm.c", RefKind::Write));
        assert!(has("orders.t", RefKind::Read));
        let call = r.iter().find(|x| x.0 == "orders_app.notify").unwrap();
        assert_eq!((call.1, call.2, call.3.as_str()), (Space::Routine, RefKind::Call, "orders_app.notify"));
        let calls = scan_plpgsql_body("perform orders_app.notify(1, f(2, 3));", 0, None, &[], false);
        assert_eq!(calls.iter().find(|c| c.name.name == "notify").unwrap().args, Some(2));
    }

    #[test]
    fn dynamic_sql_in_execute_and_format() {
        let body = "begin execute format('delete from %I.audit where id = %L', s, 1); execute 'truncate crm.log'; end";
        let r = scan_plpgsql_body(body, 100, None, &[], false);
        let log = r.iter().find(|x| x.name.to_string() == "crm.log").unwrap();
        assert!(log.dynamic && log.kind == RefKind::Write);
        assert_eq!(&body[log.span.start - 100..log.span.end - 100], "crm.log");
        assert!(!r.iter().any(|x| x.name.name == "audit"), "schema.%I is a parameter: {r:?}");
    }

    #[test]
    fn format_specs_keep_their_width() {
        assert_eq!(replace_format_specs("a %I b %1$L c %% d %-10s"), "a $1 b $1   c    d $1   ");
    }
}
