//! Errors of the PostgreSQL parser with their position. `pg_query` reports only the message of a
//! failed parse, so the failing text is parsed once more through libpg_query's C API (linked by
//! `pg_query`), whose error also carries the cursor position and the context.

use std::ffi::{CStr, CString, c_char, c_int};

#[repr(C)]
struct PgQueryError {
    message: *mut c_char,
    funcname: *mut c_char,
    filename: *mut c_char,
    lineno: c_int,
    cursorpos: c_int,
    context: *mut c_char,
}

#[repr(C)]
struct PgQueryParseResult {
    parse_tree: *mut c_char,
    stderr_buffer: *mut c_char,
    error: *mut PgQueryError,
}

#[repr(C)]
struct PgQueryPlpgsqlParseResult {
    plpgsql_funcs: *mut c_char,
    error: *mut PgQueryError,
}

unsafe extern "C" {
    fn pg_query_parse(input: *const c_char) -> PgQueryParseResult;
    fn pg_query_free_parse_result(result: PgQueryParseResult);
    fn pg_query_parse_plpgsql(input: *const c_char) -> PgQueryPlpgsqlParseResult;
    fn pg_query_free_plpgsql_parse_result(result: PgQueryPlpgsqlParseResult);
}

/// A parse error: the message, its context ("compilation of PL/pgSQL function "f" near line 2")
/// and the 1-based character position in the input (0 when the parser gives none).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ParseError {
    pub message: String,
    pub context: String,
    pub cursor: usize,
}

fn text(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: libpg_query's strings are NUL-terminated and live until the result is freed.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn read(error: *const PgQueryError) -> Option<ParseError> {
    if error.is_null() {
        return None;
    }
    // SAFETY: a non-null error points at the struct libpg_query filled in.
    let e = unsafe { &*error };
    Some(ParseError { message: text(e.message), context: text(e.context), cursor: e.cursorpos.max(0) as usize })
}

/// The error parsing `sql` as SQL; None when it parses.
pub fn sql_error(sql: &str) -> Option<ParseError> {
    let input = CString::new(sql).ok()?;
    // SAFETY: the input is a valid C string; the result is freed after it is read.
    unsafe {
        let result = pg_query_parse(input.as_ptr());
        let error = read(result.error);
        pg_query_free_parse_result(result);
        error
    }
}

/// The parse tree of `sql` as libpg_query's JSON (the C structs' field names, enums as their
/// names, byte locations), or the error with its position.
pub fn parse_json(sql: &str) -> Result<serde_json::Value, ParseError> {
    let input = CString::new(sql).map_err(|_| ParseError { message: "NUL in SQL text".into(), ..Default::default() })?;
    // SAFETY: as in `sql_error`; the tree is copied out before the result is freed.
    let (tree, error) = unsafe {
        let result = pg_query_parse(input.as_ptr());
        let error = read(result.error);
        let tree = if error.is_none() { text(result.parse_tree) } else { String::new() };
        pg_query_free_parse_result(result);
        (tree, error)
    };
    if let Some(e) = error {
        return Err(e);
    }
    serde_json::from_str(&tree).map_err(|e| ParseError { message: e.to_string(), ..Default::default() })
}

/// The error compiling the PL/pgSQL function `sql` (a CREATE FUNCTION statement); None when it
/// compiles.
pub fn plpgsql_error(sql: &str) -> Option<ParseError> {
    let input = CString::new(sql).ok()?;
    // SAFETY: as in `sql_error`.
    unsafe {
        let result = pg_query_parse_plpgsql(input.as_ptr());
        let error = read(result.error);
        pg_query_free_plpgsql_parse_result(result);
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_errors_point_at_the_token() {
        let sql = "select a from where b";
        let e = sql_error(sql).unwrap();
        assert_eq!(e.message, "syntax error at or near \"where\"");
        assert_eq!(&sql[e.cursor - 1..e.cursor + 4], "where");
        assert_eq!(sql_error("select 1"), None);
    }

    #[test]
    fn parse_trees_come_as_json_with_byte_locations() {
        let tree = parse_json("select 1; create table s.t (id int)").unwrap();
        let stmts = tree["stmts"].as_array().unwrap();
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[1]["stmt_location"], 10);
        assert_eq!(stmts[1]["stmt"]["CreateStmt"]["relation"]["relname"], "t");
        assert_eq!(parse_json("select from where").unwrap_err().cursor, 13);
    }

    #[test]
    fn plpgsql_errors_name_the_line_in_the_context() {
        let e = plpgsql_error("create function f() returns int language plpgsql as $$\nbegin\n  selec 1;\nend $$;").unwrap();
        assert_eq!(e.message, "syntax error at or near \"selec\"");
        assert!(e.context.contains("near line"), "{e:?}");
        assert_eq!(plpgsql_error("create function f() returns int language plpgsql as $$ begin return 1; end $$;"), None);
    }
}
