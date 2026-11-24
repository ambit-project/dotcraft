//! Liquibase formatted SQL and psql scripts turned into text the PostgreSQL parser accepts.
//! Every replacement keeps the byte length, so offsets in the prepared text are offsets in the
//! file.

/// A Liquibase changeset: its header and the bytes up to the next one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changeset {
    pub author: String,
    pub id: String,
    /// 0-based line of the `--changeset` header.
    pub line: usize,
    /// The `--comment:` lines, joined with newlines.
    pub comment: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    /// The text to parse: as long as the file, in bytes.
    pub sql: String,
    pub changesets: Vec<Changeset>,
    pub is_liquibase: bool,
    /// The file has psql meta-command lines (`\set`, `\i`…), blanked in `sql`.
    pub is_psql: bool,
    /// Liquibase properties used in the file (`role_prefix` for `${role_prefix}`).
    pub properties: Vec<String>,
}

/// The identifier a property becomes in the prepared text: `${role_prefix}` → `__role_prefix_`.
pub fn placeholder(property: &str) -> String {
    format!("__{}_", property.replace(['.', '-'], "_"))
}

/// Puts `${property}` back into a name read from the prepared text.
pub fn restore(name: &str, properties: &[String]) -> String {
    if properties.is_empty() || !name.contains("__") {
        return name.to_string();
    }
    let mut out = name.to_string();
    for p in properties {
        out = out.replace(&placeholder(p), &format!("${{{p}}}"));
    }
    out
}

fn is_property_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-')
}

/// `${role_prefix}` → `__role_prefix_`: an identifier of the same length.
fn replace_properties(text: &str, properties: &mut Vec<String>) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && bytes.get(i + 1) == Some(&b'{') {
            let mut j = i + 2;
            while j < bytes.len() && is_property_char(bytes[j]) {
                j += 1;
            }
            if j > i + 2 && bytes.get(j) == Some(&b'}') {
                let name = String::from_utf8_lossy(&bytes[i + 2..j]).into_owned();
                if !properties.contains(&name) {
                    properties.push(name);
                }
                out.extend_from_slice(b"__");
                out.extend(bytes[i + 2..j].iter().map(|&c| if c == b'.' || c == b'-' { b'_' } else { c }));
                out.push(b'_');
                i = j + 1;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

/// `--changeset author:id attr:value…` → (author, id).
fn changeset_header(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("--")?.trim_start().strip_prefix("changeset")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let head = rest.split_whitespace().next()?;
    let (author, id) = head.split_once(':')?;
    (!author.is_empty() && !id.is_empty()).then(|| (author.to_string(), id.to_string()))
}

fn comment_line(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("--")?.trim_start().strip_prefix("comment:")?;
    Some(rest.strip_prefix(' ').unwrap_or(rest).trim_end())
}

pub fn prepare(text: &str) -> Prepared {
    // libpg_query takes C strings.
    let text = if text.contains('\0') { text.replace('\0', " ") } else { text.to_string() };
    let mut properties = Vec::new();
    let sql = replace_properties(&text, &mut properties);
    let is_liquibase = text.trim_start().to_ascii_lowercase().starts_with("--liquibase formatted sql");
    let mut changesets = Vec::new();
    let mut open: Option<(Changeset, Vec<String>)> = None;
    let mut out = String::with_capacity(sql.len());
    let mut is_psql = false;
    let mut offset = 0;
    for (line_no, raw) in sql.split_inclusive('\n').enumerate() {
        let line = raw.trim_end_matches(['\n', '\r']);
        let mut written = std::borrow::Cow::Borrowed(raw);
        if is_liquibase {
            if let Some((author, id)) = changeset_header(line) {
                if let Some((mut c, comments)) = open.take() {
                    c.end = offset;
                    c.comment = comments.join("\n");
                    changesets.push(c);
                }
                open = Some((Changeset { author, id, line: line_no, comment: String::new(), start: offset, end: 0 }, Vec::new()));
            } else if let Some(c) = comment_line(line)
                && let Some((_, comments)) = open.as_mut()
            {
                comments.push(c.to_string());
            }
        } else if line.starts_with('\\') {
            is_psql = true;
            written = std::borrow::Cow::Owned(" ".repeat(line.len()) + &raw[line.len()..]);
        }
        out.push_str(&written);
        offset += raw.len();
    }
    if let Some((mut c, comments)) = open.take() {
        c.end = sql.len();
        c.comment = comments.join("\n");
        changesets.push(c);
    }
    if is_psql {
        out = blank_psql_variables(&out);
    }
    debug_assert_eq!(out.len(), text.len(), "prepared text keeps the offsets of the file");
    Prepared { sql: out, changesets, is_liquibase, is_psql, properties }
}

/// psql variables (`:name`, `:'name'`, `:"name"`): the colon becomes a space, so the rest reads
/// as a name or a literal.
fn blank_psql_variables(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    for i in 0..bytes.len() {
        if bytes[i] != b':' {
            continue;
        }
        let before = i.checked_sub(1).map(|j| bytes[j]);
        if before.is_some_and(|b| b == b':' || b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        if bytes.get(i + 1).is_some_and(|&n| n == b'\'' || n == b'"' || n.is_ascii_alphabetic() || n == b'_') {
            out[i] = b' ';
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liquibase_changesets_and_properties_keep_offsets() {
        let text = "--liquibase formatted sql\n\n--changeset ada:20260101-01 runOnChange:true\n--comment: Orders\n--comment: and lines\ncreate table ${schema}.t (id int);\n--changeset bob:2\nselect 1;\n";
        let p = prepare(text);
        assert_eq!(p.sql.len(), text.len());
        assert!(p.is_liquibase && !p.is_psql);
        assert!(p.sql.contains("create table __schema_.t"));
        assert_eq!(p.properties, ["schema"]);
        assert_eq!(restore("__schema_", &p.properties), "${schema}");
        assert_eq!(p.changesets.len(), 2);
        let first = &p.changesets[0];
        assert_eq!((first.author.as_str(), first.id.as_str(), first.line), ("ada", "20260101-01", 2));
        assert_eq!(first.comment, "Orders\nand lines");
        assert_eq!(&text[first.start..first.start + 12], "--changeset ");
        assert_eq!(first.end, p.changesets[1].start);
        assert_eq!(p.changesets[1].end, text.len());
    }

    #[test]
    fn psql_meta_commands_and_variables_are_blanked() {
        let text = "\\set ON_ERROR_STOP on\r\nselect :'name', a::int from t where x = :id;\n";
        let p = prepare(text);
        assert!(p.is_psql);
        assert_eq!(p.sql.len(), text.len());
        assert!(p.sql.starts_with("                     \r\n"));
        assert!(p.sql.contains("select  'name', a::int"));
        assert!(p.sql.contains("x =  id;"));
    }
}
