//! Field-qualified query parsing for `search`.
//!
//! Lets an agent narrow a search inline instead of via separate flags:
//!
//! ```text
//! lang:rust path:src/auth name:login how is the token refreshed
//! ```
//!
//! splits into structured filters (`lang=rust`, path-substring `src/auth`,
//! filename-substring `login`) plus the free text (`how is the token
//! refreshed`) that goes to the hybrid retriever. Filters narrow the candidate
//! set; the free text scores within it.
//!
//! Recognised fields (case-insensitive, value is one whitespace-delimited
//! token; quote with `"..."` to include spaces):
//!   - `lang:` / `language:` — chunk language (e.g. `rust`, `python`).
//!   - `path:` — case-insensitive substring of the chunk's repo-relative path.
//!   - `name:` — case-insensitive substring of the file's name (stem).
//!
//! Unknown prefixes (`TODO:`) pass through as plain text, so searching for a
//! literal `foo:` still works. Repeated fields OR together
//! (`lang:rust lang:go` → either language).

/// Structured filters peeled out of a raw query, plus the remaining free text.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParsedQuery {
    /// Free-text portion fed to the retriever. May be empty.
    pub text: String,
    /// `lang:` / `language:` values (lowercased). OR'd.
    pub languages: Vec<String>,
    /// `path:` substrings (lowercased). OR'd.
    pub paths: Vec<String>,
    /// `name:` (filename/stem) substrings (lowercased). OR'd.
    pub names: Vec<String>,
}

impl ParsedQuery {
    /// True when any field filter was present (i.e. the query wasn't plain
    /// text). Currently exercised by tests; handy for callers that want to
    /// branch on "did the user narrow this query?".
    #[allow(dead_code)]
    pub fn has_filters(&self) -> bool {
        !self.languages.is_empty() || !self.paths.is_empty() || !self.names.is_empty()
    }
}

/// Parse a raw query into field filters + free text. Never fails: anything
/// not recognised as a `field:value` token is treated as free text.
pub fn parse_query(raw: &str) -> ParsedQuery {
    let mut pq = ParsedQuery::default();
    let mut text_tokens: Vec<String> = Vec::new();

    for tok in tokenize(raw) {
        match split_field(&tok) {
            Some((field, value)) if !value.is_empty() => {
                let v = value.to_ascii_lowercase();
                match field.to_ascii_lowercase().as_str() {
                    "lang" | "language" => pq.languages.push(v),
                    "path" => pq.paths.push(v),
                    "name" => pq.names.push(v),
                    // Unknown field → keep the whole token as free text so
                    // `TODO:` / `http://x` still searches literally.
                    _ => text_tokens.push(tok),
                }
            }
            _ => text_tokens.push(tok),
        }
    }
    pq.text = text_tokens.join(" ");
    pq
}

/// Whitespace tokenizer that keeps `"quoted segments"` (including a
/// `field:"quoted value"` form) as a single token, with the quotes stripped.
fn tokenize(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in raw.chars() {
        match c {
            '"' => in_quote = !in_quote,
            c if c.is_whitespace() && !in_quote => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `field:value` → `Some(("field", "value"))`. A leading-colon or
/// no-colon token returns `None`. The value keeps everything after the
/// first colon (so `path:src/a:b` → value `src/a:b`).
fn split_field(tok: &str) -> Option<(&str, &str)> {
    let idx = tok.find(':')?;
    if idx == 0 {
        return None;
    }
    Some((&tok[..idx], &tok[idx + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_query_has_no_filters() {
        let pq = parse_query("how does login work");
        assert_eq!(pq.text, "how does login work");
        assert!(!pq.has_filters());
    }

    #[test]
    fn extracts_all_three_fields() {
        let pq = parse_query("lang:Rust path:src/auth name:Login refresh the token");
        assert_eq!(pq.languages, vec!["rust"]);
        assert_eq!(pq.paths, vec!["src/auth"]);
        assert_eq!(pq.names, vec!["login"]);
        assert_eq!(pq.text, "refresh the token");
        assert!(pq.has_filters());
    }

    #[test]
    fn language_alias_and_repeats_or() {
        let pq = parse_query("language:rust lang:go HandlerStack");
        assert_eq!(pq.languages, vec!["rust", "go"]);
        assert_eq!(pq.text, "HandlerStack");
    }

    #[test]
    fn unknown_prefix_stays_text() {
        let pq = parse_query("TODO: fix the parser");
        assert!(!pq.has_filters());
        assert_eq!(pq.text, "TODO: fix the parser");
    }

    #[test]
    fn quoted_value_keeps_spaces() {
        let pq = parse_query("path:\"src/some path\" parse");
        assert_eq!(pq.paths, vec!["src/some path"]);
        assert_eq!(pq.text, "parse");
    }

    #[test]
    fn empty_value_is_text() {
        // `name:` with no value isn't a filter.
        let pq = parse_query("name: thing");
        assert!(pq.names.is_empty());
        assert_eq!(pq.text, "name: thing");
    }
}
