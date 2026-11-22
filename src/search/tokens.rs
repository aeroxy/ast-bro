//! Identifier-aware tokenization for BM25 indexing.
//!
//! Splits compound identifiers (camelCase,
//! PascalCase, snake_case) into sub-tokens so partial matches work, while
//! preserving the original compound for exact-match boosting.

use regex::Regex;
use std::sync::OnceLock;

fn ident_re() -> &'static Regex {
  static RE: OnceLock<Regex> = OnceLock::new();
  RE.get_or_init(|| Regex::new(r"[a-zA-Z_][a-zA-Z0-9_]*").unwrap())
}

/// Iterate identifier-shaped runs in `text`.
///
/// Equivalent to the Python regex `[a-zA-Z_][a-zA-Z0-9_]*`: must start with an
/// ASCII letter or underscore, then continue with letters / digits / underscore.
pub fn scan_identifiers(text: &str) -> Vec<&str> {
  ident_re().find_iter(text).map(|m| m.as_str()).collect()
}

/// Split a single identifier into its sub-tokens via camelCase / snake_case.
///
/// Returns **only** the sub-tokens (lowercased), not the original. Callers that
/// also want the original lowered string must push it themselves — `tokenize`
/// does this.
///
/// `"HandlerStack"` -> `["handler", "stack"]`
/// `"my_func"`      -> `["my", "func"]`
/// `"simple"`       -> `[]`           (single part: no sub-tokens to add)
/// `"_foo_"`        -> `[]`           (single non-empty part after split)
pub fn split_identifier(token: &str) -> Vec<String> {
  let parts: Vec<String> = if token.contains('_') {
    token
      .split('_')
      .filter(|p| !p.is_empty())
      .map(str::to_ascii_lowercase)
      .collect()
  } else {
    camel_split(token)
      .into_iter()
      .map(|p| p.to_ascii_lowercase())
      .collect()
  };

  if parts.len() >= 2 {
    parts
  } else {
    Vec::new()
  }
}

/// Split text into lowercase identifier-like tokens for BM25 indexing.
///
/// For each identifier in `text` we emit the lowercased compound, followed by
/// any sub-tokens. Compound identifiers (camelCase, PascalCase, snake_case) get
/// expanded so partial matches work; the original compound stays so exact-match
/// boosting can find it.
pub fn tokenize(text: &str) -> Vec<String> {
  let mut result = Vec::new();
  for ident in scan_identifiers(text) {
    result.push(ident.to_ascii_lowercase());
    result.extend(split_identifier(ident));
  }
  result
}

/// Split an ASCII identifier on camelCase / PascalCase / digit boundaries.
///
/// Equivalent to the Python regex `[A-Z]+(?=[A-Z][a-z])|[A-Z]?[a-z]+|[A-Z]+|[0-9]+`,
/// hand-rolled because lookahead isn't worth a regex dep for one site.
///
/// "HandlerStack"    -> ["Handler", "Stack"]
/// "getHTTPResponse" -> ["get", "HTTP", "Response"]
/// "XMLParser"       -> ["XML", "Parser"]
fn camel_split(s: &str) -> Vec<String> {
  let chars: Vec<char> = s.chars().collect();
  let n = chars.len();
  let mut out = Vec::new();
  let mut i = 0;
  while i < n {
    let c = chars[i];
    if c.is_ascii_digit() {
      let start = i;
      while i < n && chars[i].is_ascii_digit() {
        i += 1;
      }
      out.push(chars[start..i].iter().collect());
    } else if c.is_ascii_uppercase() {
      let start = i;
      let mut end = i;
      while end < n && chars[end].is_ascii_uppercase() {
        end += 1;
      }
      // Three cases mirror the Python regex alternation order:
      // 1. Upper run ≥2 followed by lowercase → split off acronym, leave the
      //    last upper for the next word ("XMLParser" → "XML" + "Parser").
      // 2. Pure upper run, no following lowercase → emit as-is ("HTTP").
      // 3. Single upper, then lowers → emit "Upper+lowers" ("Handler").
      if end - start >= 2 && end < n && chars[end].is_ascii_lowercase() {
        let acronym_end = end - 1;
        out.push(chars[start..acronym_end].iter().collect());
        i = acronym_end;
      } else if end - start >= 2 {
        out.push(chars[start..end].iter().collect());
        i = end;
      } else {
        let mut j = end;
        while j < n && chars[j].is_ascii_lowercase() {
          j += 1;
        }
        out.push(chars[start..j].iter().collect());
        i = j;
      }
    } else if c.is_ascii_lowercase() {
      let start = i;
      while i < n && chars[i].is_ascii_lowercase() {
        i += 1;
      }
      out.push(chars[start..i].iter().collect());
    } else {
      i += 1;
    }
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  // ── split_identifier (sub-tokens only) ────────────────────────────────

  #[test]
  fn split_handler_stack() {
    assert_eq!(split_identifier("HandlerStack"), vec!["handler", "stack"]);
  }

  #[test]
  fn split_snake_case() {
    assert_eq!(split_identifier("my_func"), vec!["my", "func"]);
  }

  #[test]
  fn split_simple_returns_empty() {
    assert!(split_identifier("simple").is_empty());
  }

  #[test]
  fn split_acronym_in_camel() {
    assert_eq!(
      split_identifier("getHTTPResponse"),
      vec!["get", "http", "response"]
    );
  }

  #[test]
  fn split_pure_acronym_compound() {
    assert_eq!(split_identifier("XMLParser"), vec!["xml", "parser"]);
  }

  #[test]
  fn split_with_digits() {
    assert_eq!(split_identifier("h2o"), vec!["h", "2", "o"]);
  }

  #[test]
  fn split_leading_trailing_underscore() {
    // "_foo_" → split on "_" gives ["", "foo", ""] → filter → ["foo"]
    // Single non-empty part → no sub-tokens.
    assert!(split_identifier("_foo_").is_empty());
    // Two-part snake with surrounding underscores still emits both parts.
    assert_eq!(split_identifier("_foo_bar_"), vec!["foo", "bar"]);
  }

  // ── tokenize (compound + sub-tokens) ──────────────────────────────────

  #[test]
  fn tokenize_full_text() {
    let toks = tokenize("def getUserById(user_id):");
    // "def"         → ["def"]
    // "getUserById" → ["getuserbyid", "get", "user", "by", "id"]
    // "user_id"     → ["user_id", "user", "id"]
    assert_eq!(
      toks,
      vec![
        "def",
        "getuserbyid",
        "get",
        "user",
        "by",
        "id",
        "user_id",
        "user",
        "id"
      ]
    );
  }

  #[test]
  fn tokenize_skips_punctuation_and_numbers() {
    // Standalone digits don't start an identifier (must lead with alpha or _).
    assert_eq!(tokenize("foo + 42 - bar"), vec!["foo", "bar"]);
  }

  #[test]
  fn tokenize_simple_word() {
    // "simple" → split returns [], tokenize prepends the lowered original.
    assert_eq!(tokenize("simple"), vec!["simple"]);
  }

  // ── scan_identifiers ──────────────────────────────────────────────────

  #[test]
  fn scan_identifiers_basic() {
    assert_eq!(
      scan_identifiers("a + b1c (d_e) 2f"),
      vec!["a", "b1c", "d_e", "f"]
    );
  }

  #[test]
  fn scan_identifiers_underscore_start() {
    assert_eq!(scan_identifiers("_x __y_z"), vec!["_x", "__y_z"]);
  }
}
