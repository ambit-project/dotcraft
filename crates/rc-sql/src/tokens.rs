//! Tokens of PostgreSQL's own scanner, with helpers for names: qualified names, the name after a
//! keyword, matching parentheses. Offsets are bytes of the scanned text.

use pg_query::protobuf::Token;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tok {
    pub start: usize,
    pub end: usize,
    /// The scanner's token (`pg_query::protobuf::Token`; a single character is its ASCII code).
    pub token: i32,
    /// 0: not a keyword; 1 unreserved, 2 column name, 3 type or function name, 4 reserved.
    pub keyword: u8,
}

impl Tok {
    pub fn is_comment(&self) -> bool {
        self.token == Token::SqlComment as i32 || self.token == Token::CComment as i32
    }

    pub fn is_string(&self) -> bool {
        [Token::Sconst, Token::Usconst, Token::Bconst, Token::Xconst].iter().any(|t| *t as i32 == self.token)
    }

    pub fn is_number(&self) -> bool {
        self.token == Token::Iconst as i32 || self.token == Token::Fconst as i32
    }

    /// A name: an identifier, or a keyword that is not reserved (those can be names).
    pub fn is_name(&self) -> bool {
        self.token == Token::Ident as i32 || self.token == Token::Uident as i32 || (1..=3).contains(&self.keyword)
    }

    pub fn is_keyword(&self) -> bool {
        self.keyword != 0
    }
}

/// Scans `text`; an unterminated string, quoted name or comment (while typing) is closed at the
/// end so the rest still scans. Empty when nothing helps.
pub fn scan(text: &str) -> Vec<Tok> {
    let convert = |tokens: Vec<pg_query::protobuf::ScanToken>| -> Vec<Tok> {
        tokens
            .into_iter()
            .filter(|t| (t.start as usize) < text.len().max(1))
            .map(|t| Tok { start: t.start as usize, end: (t.end as usize).min(text.len()), token: t.token, keyword: t.keyword_kind as u8 })
            .collect()
    };
    if let Ok(tokens) = pg_query::scan_tokens(text) {
        return convert(tokens);
    }
    for suffix in ["'", "*/", "\"", "$$", "'*/", "*/'"] {
        if let Ok(tokens) = pg_query::scan_tokens(&format!("{text}{suffix}")) {
            return convert(tokens);
        }
    }
    // An unterminated $tag$: close it with its own tag.
    if let Some(tag) = open_dollar_tag(text)
        && let Ok(tokens) = pg_query::scan_tokens(&format!("{text}{tag}"))
    {
        return convert(tokens);
    }
    Vec::new()
}

/// The last `$tag$` opener in `text` (best effort, for closing it).
fn open_dollar_tag(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut found = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let mut j = i + 1;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'$' && !bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
                found = Some(text[i..=j].to_string());
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    found
}

/// A qualified name `a.b.c` in tokens `first..=last`, spanning bytes `start..end`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub parts: Vec<String>,
    pub first: usize,
    pub last: usize,
    pub start: usize,
    pub end: usize,
}

/// Noise between an object keyword and its name (`CREATE INDEX CONCURRENTLY IF NOT EXISTS ix`).
const NAME_NOISE: &[&str] = &["if", "not", "exists", "concurrently", "only", "or", "replace"];

/// The tokens of a text without its comments.
pub struct Tokens<'a> {
    pub text: &'a str,
    pub toks: Vec<Tok>,
    pub comments: Vec<Tok>,
}

impl<'a> Tokens<'a> {
    pub fn new(text: &'a str) -> Self {
        let (comments, toks) = scan(text).into_iter().partition(Tok::is_comment);
        Tokens { text, toks, comments }
    }

    pub fn text_of(&self, t: &Tok) -> &'a str {
        self.text.get(t.start..t.end).unwrap_or("")
    }

    /// Lower-case text of token `i` ("" past the end).
    pub fn word(&self, i: usize) -> String {
        self.toks.get(i).map(|t| self.text_of(t).to_ascii_lowercase()).unwrap_or_default()
    }

    /// The name a token stands for: unquoted names fold to lower case, quoted ones keep it.
    pub fn name_of(&self, t: &Tok) -> String {
        let raw = self.text_of(t);
        if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
            raw[1..raw.len() - 1].replace("\"\"", "\"")
        } else {
            raw.to_lowercase()
        }
    }

    pub fn is_symbol(&self, i: usize, symbol: &str) -> bool {
        self.toks.get(i).is_some_and(|t| !t.is_name() && self.text_of(t) == symbol)
    }

    /// Index of the first token starting at or after `offset`.
    pub fn index_at(&self, offset: usize) -> usize {
        self.toks.partition_point(|t| t.start < offset)
    }

    /// `name(.name)*` from token `i`.
    pub fn qualified_at(&self, i: usize) -> Option<Run> {
        let first = self.toks.get(i).filter(|t| t.is_name())?;
        let mut parts = vec![self.name_of(first)];
        let mut last = i;
        while self.is_symbol(last + 1, ".") && self.toks.get(last + 2).is_some_and(Tok::is_name) {
            parts.push(self.name_of(&self.toks[last + 2]));
            last += 2;
        }
        Some(Run { parts, first: i, last, start: first.start, end: self.toks[last].end })
    }

    /// The qualified name whose first token starts at `offset`.
    pub fn qualified_starting_at(&self, offset: usize) -> Option<Run> {
        let i = self.index_at(offset);
        self.toks.get(i).filter(|t| t.start == offset)?;
        self.qualified_at(i)
    }

    /// The first name after one of `keywords` (lower case) within `start..end`, skipping
    /// `IF NOT EXISTS`, `OR REPLACE`, `CONCURRENTLY`, `ONLY`.
    pub fn name_after(&self, keywords: &[&str], start: usize, end: usize) -> Option<Run> {
        let mut i = self.index_at(start);
        while i < self.toks.len() && self.toks[i].start < end {
            if self.toks[i].is_keyword() && keywords.contains(&self.word(i).as_str()) {
                let mut j = i + 1;
                while j < self.toks.len() && NAME_NOISE.contains(&self.word(j).as_str()) {
                    j += 1;
                }
                if let Some(run) = self.qualified_at(j).filter(|r| r.end <= end) {
                    return Some(run);
                }
            }
            i += 1;
        }
        None
    }

    /// The first run of tokens spelling `parts` (or just its last part) within `start..end`.
    pub fn find(&self, parts: &[String], start: usize, end: usize) -> Option<Run> {
        let mut i = self.index_at(start);
        while i < self.toks.len() && self.toks[i].start < end {
            if (i == 0 || !self.is_symbol(i - 1, "."))
                && let Some(run) = self.qualified_at(i)
                && (run.parts == parts || (run.parts.len() == 1 && parts.last() == run.parts.last()))
            {
                return Some(run);
            }
            i += 1;
        }
        None
    }

    /// Index of the `)` matching the `(` at token `open` (the last token when unbalanced).
    pub fn matching_paren(&self, open: usize) -> usize {
        let mut depth = 0i32;
        for j in open..self.toks.len() {
            match self.text_of(&self.toks[j]) {
                "(" => depth += 1,
                ")" => {
                    depth -= 1;
                    if depth == 0 {
                        return j;
                    }
                }
                _ => {}
            }
        }
        self.toks.len().saturating_sub(1)
    }

    /// Arguments between the `(` at `open` and its `)` at `close`, counted at depth 0.
    pub fn arg_count(&self, open: usize, close: usize) -> usize {
        if close <= open + 1 {
            return 0;
        }
        let mut depth = 0i32;
        let mut count = 1;
        for j in open + 1..close {
            match self.text_of(&self.toks[j]) {
                "(" | "[" => depth += 1,
                ")" | "]" => depth -= 1,
                "," if depth == 0 => count += 1,
                _ => {}
            }
        }
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_fold_and_qualify() {
        let t = Tokens::new("CREATE OR REPLACE FUNCTION Orders_App.\"Place\"(x int) -- note\n");
        let run = t.name_after(&["function"], 0, t.text.len()).unwrap();
        assert_eq!(run.parts, ["orders_app", "Place"]);
        assert_eq!(&t.text[run.start..run.end], "Orders_App.\"Place\"");
        assert_eq!(t.comments.len(), 1);
        let open = run.last + 1;
        assert_eq!(t.arg_count(open, t.matching_paren(open)), 1);
    }

    #[test]
    fn unterminated_text_still_scans() {
        for text in ["select 'abc", "select 1 /* open", "select \"Name", "do $body$ begin"] {
            assert!(!scan(text).is_empty(), "{text}");
        }
        let toks = scan("select 'abc");
        assert_eq!(toks.last().unwrap().end, "select 'abc".len());
    }
}
