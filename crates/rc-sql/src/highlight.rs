//! Syntax classes of a SQL file from the PostgreSQL scanner; dollar-quoted bodies (functions,
//! DO blocks) are scanned again inside.

use crate::prepare::prepare;
use crate::tokens::{Tok, scan};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlKind {
    Keyword,
    Ident,
    String,
    Number,
    Comment,
    /// `--changeset`, `--comment:`, `--rollback`… of Liquibase formatted SQL.
    Liquibase,
    Operator,
    Punct,
    /// `$1` and psql's variables.
    Param,
}

/// Byte ranges of `text` with their class, sorted.
pub fn highlight(text: &str) -> Vec<(usize, usize, HlKind)> {
    let prepared = prepare(text);
    let mut out = Vec::new();
    scan_into(&prepared.sql, 0, &mut out, 0);
    out
}

fn is_liquibase(comment: &str) -> bool {
    let rest = comment.trim_start_matches('-').trim_start();
    comment.starts_with("--") && ["changeset", "comment:", "liquibase", "rollback", "precondition"].iter().any(|p| rest.starts_with(p))
}

fn scan_into(text: &str, base: usize, out: &mut Vec<(usize, usize, HlKind)>, depth: usize) {
    for t in scan(text) {
        let Tok { start, end, .. } = t;
        let raw = &text[start..end];
        let kind = if t.is_comment() {
            if is_liquibase(raw) { HlKind::Liquibase } else { HlKind::Comment }
        } else if t.is_string() {
            if let Some(tag) = dollar_tag(raw).filter(|tag| depth < 3 && raw.len() >= tag.len() * 2) {
                let inner = (start + tag.len(), end - tag.len());
                out.push((base + start, base + inner.0, HlKind::String));
                scan_into(&text[inner.0..inner.1], base + inner.0, out, depth + 1);
                out.push((base + inner.1, base + end, HlKind::String));
                continue;
            }
            HlKind::String
        } else if t.is_number() {
            HlKind::Number
        } else if t.token == pg_query::protobuf::Token::Param as i32 {
            HlKind::Param
        } else if t.is_keyword() {
            HlKind::Keyword
        } else if t.is_name() {
            HlKind::Ident
        } else if raw.len() == 1 && "(),;.[]".contains(raw) {
            HlKind::Punct
        } else {
            HlKind::Operator
        };
        out.push((base + start, base + end, kind));
    }
}

/// `$body$` of a dollar-quoted string.
fn dollar_tag(raw: &str) -> Option<&str> {
    let rest = raw.strip_prefix('$')?;
    let close = rest.find('$')?;
    let tag = &raw[..close + 2];
    (raw.ends_with(tag) && tag[1..tag.len() - 1].chars().all(|c| c.is_alphanumeric() || c == '_')).then_some(tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<(&str, HlKind)> {
        highlight(text).into_iter().map(|(s, e, k)| (&text[s..e], k)).collect()
    }

    #[test]
    fn keywords_strings_numbers_and_liquibase_headers() {
        let k = kinds("--changeset ada:1\n-- plain\nselect 'a', 42 from orders.t where id = $1;");
        assert_eq!(k[0], ("--changeset ada:1", HlKind::Liquibase));
        assert_eq!(k[1], ("-- plain", HlKind::Comment));
        assert!(k.contains(&("select", HlKind::Keyword)));
        assert!(k.contains(&("'a'", HlKind::String)));
        assert!(k.contains(&("42", HlKind::Number)));
        assert!(k.contains(&("orders", HlKind::Ident)));
        assert!(k.contains(&("$1", HlKind::Param)));
        assert!(k.contains(&(";", HlKind::Punct)));
    }

    #[test]
    fn function_bodies_are_highlighted_inside() {
        let text = "create function f() returns int language plpgsql as $fn$\nbegin\n  return 1; -- one\nend $fn$;";
        let k = kinds(text);
        assert!(k.contains(&("$fn$", HlKind::String)));
        assert!(k.contains(&("begin", HlKind::Keyword)));
        assert!(k.contains(&("-- one", HlKind::Comment)));
        assert!(k.contains(&("1", HlKind::Number)));
    }

    #[test]
    fn multibyte_text_keeps_byte_ranges() {
        let text = "select 'zażółć' as gęś;";
        for (s, e, _) in highlight(text) {
            assert!(text.is_char_boundary(s) && text.is_char_boundary(e));
        }
    }
}
