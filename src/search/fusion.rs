//! RRF fusion + alpha resolver for hybrid search.
//!
//! - **RRF (Reciprocal Rank Fusion)**: `score = 1 / (k + rank)` with `k = 60`.
//!   Used to normalize semantic and BM25 scores into the same magnitude band
//!   before combining, so the alpha weight has a consistent meaning regardless
//!   of which backend produced the raw scores.
//! - **Alpha resolver**: `0.3` for symbol queries (BM25-leaning, exact keyword
//!   matters), `0.5` for natural-language (balanced semantic + BM25). Override
//!   with an explicit alpha if you know better.
//! - **`is_symbol_query`**: heuristic for "this looks like an identifier the
//!   user typed verbatim" vs. an English question.

use regex::Regex;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Reciprocal Rank Fusion constant. Higher k flattens the curve.
pub const RRF_K: u32 = 60;

const ALPHA_SYMBOL: f32 = 0.3;
const ALPHA_NL: f32 = 0.5;

fn symbol_query_re() -> &'static Regex {
  static RE: OnceLock<Regex> = OnceLock::new();
  RE.get_or_init(|| {
    // Rust raw strings don't honour `\` line continuation like Python does;
    // build the alternation by concatenation so each branch stays readable.
    let pattern = concat!(
      "^(?:",
      r"[A-Za-z_][A-Za-z0-9_]*(?:(?:::|\\|->|\.)[A-Za-z_][A-Za-z0-9_]*)+", // namespace-qualified
      "|",
      r"_[A-Za-z0-9_]*", // leading underscore
      "|",
      r"[A-Za-z][A-Za-z0-9]*[A-Z_][A-Za-z0-9_]*", // contains uppercase or underscore
      "|",
      r"[A-Z][A-Za-z0-9]*", // starts with uppercase
      ")$",
    );
    Regex::new(pattern).expect("symbol_query_re")
  })
}

/// Return `true` if `query` looks like a bare symbol or namespace-qualified
/// identifier (e.g. `HandlerStack`, `_dunder`, `Sinatra::Base`, `app.use`).
/// Plain lowercase words like `"session"` are treated as natural language.
pub fn is_symbol_query(query: &str) -> bool {
  symbol_query_re().is_match(query.trim())
}

/// Pick the semantic-vs-BM25 blend weight for a given query.
///
/// Returns `0.3` for symbol queries (BM25-heavy), `0.5` for NL queries
/// (balanced). Caller-supplied `alpha` always wins.
pub fn resolve_alpha(query: &str, alpha: Option<f32>) -> f32 {
  match alpha {
    Some(a) => a,
    None => {
      if is_symbol_query(query) {
        ALPHA_SYMBOL
      } else {
        ALPHA_NL
      }
    }
  }
}

/// Convert raw scores into RRF-normalized scores.
///
/// `scored` is a slice of `(chunk_id, raw_score)` pairs. Higher raw scores
/// rank first. Output is a map `chunk_id → 1 / (k + rank)` where `rank` is
/// 1-indexed so the best chunk gets `1 / (k + 1)`.
///
/// Empty input → empty output.
pub fn rrf_scores(scored: &[(u32, f32)]) -> HashMap<u32, f32> {
  if scored.is_empty() {
    return HashMap::new();
  }
  // Sort by score desc; preserve original input order on ties (stable sort).
  let mut idx: Vec<usize> = (0..scored.len()).collect();
  idx.sort_by(|&a, &b| {
    scored[b]
      .1
      .partial_cmp(&scored[a].1)
      .unwrap_or(std::cmp::Ordering::Equal)
  });
  let mut out = HashMap::with_capacity(scored.len());
  for (rank0, i) in idx.into_iter().enumerate() {
    let rank = (rank0 + 1) as u32;
    out.insert(scored[i].0, 1.0 / (RRF_K + rank) as f32);
  }
  out
}

/// Combine RRF-normalized semantic and BM25 score maps with weight `alpha`.
///
/// `combined[id] = alpha * semantic.get(id, 0) + (1-alpha) * bm25.get(id, 0)`
/// over the union of keys.
pub fn combine(
  semantic: &HashMap<u32, f32>,
  bm25: &HashMap<u32, f32>,
  alpha: f32,
) -> HashMap<u32, f32> {
  let mut out = HashMap::with_capacity(semantic.len() + bm25.len());
  for (&id, &s) in semantic {
    out.insert(id, alpha * s);
  }
  for (&id, &b) in bm25 {
    let entry = out.entry(id).or_insert(0.0);
    *entry += (1.0 - alpha) * b;
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn symbol_query_pascal() {
    assert!(is_symbol_query("HandlerStack"));
    assert!(is_symbol_query("Client"));
  }

  #[test]
  fn symbol_query_namespaced() {
    assert!(is_symbol_query("Sinatra::Base"));
    assert!(is_symbol_query("app.use"));
    assert!(is_symbol_query("Foo->bar"));
    assert!(is_symbol_query(r"My\Namespace\Class"));
  }

  #[test]
  fn symbol_query_dunder_and_camel() {
    assert!(is_symbol_query("_internal"));
    assert!(is_symbol_query("getUserById"));
    assert!(is_symbol_query("snake_case_thing"));
  }

  #[test]
  fn nl_query_lowercase_word() {
    // Plain lowercase words are NL, not symbols.
    assert!(!is_symbol_query("session"));
    assert!(!is_symbol_query("how do i do x"));
    assert!(!is_symbol_query("authentication"));
  }

  #[test]
  fn resolve_alpha_explicit_wins() {
    assert_eq!(resolve_alpha("HandlerStack", Some(0.9)), 0.9);
    assert_eq!(resolve_alpha("hello world", Some(0.1)), 0.1);
  }

  #[test]
  fn resolve_alpha_auto() {
    assert_eq!(resolve_alpha("HandlerStack", None), ALPHA_SYMBOL);
    assert_eq!(resolve_alpha("how to do x", None), ALPHA_NL);
  }

  #[test]
  fn rrf_scores_empty() {
    assert!(rrf_scores(&[]).is_empty());
  }

  #[test]
  fn rrf_scores_basic() {
    // Three docs, descending raw scores: 0, 1, 2.
    let scored = [(10, 0.9), (20, 0.5), (30, 0.1)];
    let out = rrf_scores(&scored);
    assert_eq!(out.len(), 3);
    assert!((out[&10] - 1.0 / (RRF_K + 1) as f32).abs() < 1e-7);
    assert!((out[&20] - 1.0 / (RRF_K + 2) as f32).abs() < 1e-7);
    assert!((out[&30] - 1.0 / (RRF_K + 3) as f32).abs() < 1e-7);
  }

  #[test]
  fn rrf_scores_unsorted_input_is_resorted() {
    // Same data as above but provided out-of-order.
    let scored = [(20, 0.5), (30, 0.1), (10, 0.9)];
    let out = rrf_scores(&scored);
    // Best (id 10, score 0.9) gets rank 1.
    assert!(out[&10] > out[&20]);
    assert!(out[&20] > out[&30]);
  }

  #[test]
  fn combine_weights_correctly() {
    let mut sem = HashMap::new();
    sem.insert(1, 1.0);
    sem.insert(2, 0.5);
    let mut bm = HashMap::new();
    bm.insert(2, 0.4);
    bm.insert(3, 0.6);

    // alpha = 0.5: sem and bm each contribute half.
    let out = combine(&sem, &bm, 0.5);
    assert_eq!(out.len(), 3);
    assert!((out[&1] - 0.5 * 1.0).abs() < 1e-6);
    assert!((out[&2] - (0.5 * 0.5 + 0.5 * 0.4)).abs() < 1e-6);
    assert!((out[&3] - 0.5 * 0.6).abs() < 1e-6);
  }

  #[test]
  fn combine_alpha_one_is_pure_semantic() {
    let mut sem = HashMap::new();
    sem.insert(1, 1.0);
    let mut bm = HashMap::new();
    bm.insert(1, 0.5);
    bm.insert(2, 0.5);

    let out = combine(&sem, &bm, 1.0);
    // BM25 contribution multiplied by 0; only semantic survives for id 1.
    // id 2 still appears (entry created via bm path) but with score 0.
    assert_eq!(out.len(), 2);
    assert!((out[&1] - 1.0).abs() < 1e-6);
    assert!(out[&2].abs() < 1e-6);
  }
}
