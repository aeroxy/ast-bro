//! Sparse BM25 index.
//!
//! Port of the parts of `bm25s.BM25` (default "lucene" variant) with one extra: `get_scores`
//! accepts an optional boolean mask that is
//! applied as a *post-filter score multiplier* (not a slice). Multiplying after
//! scoring preserves the IDF normalization computed against the full corpus —
//! see `bm25s.get_scores(weight_mask=...)` semantics.
//!
//! Formula (lucene-style Okapi BM25):
//! ```text
//! idf(t) = log(1 + (N - df + 0.5) / (df + 0.5))
//! score(d, q) = Σ_{t ∈ q} idf(t) · tf(t,d) · (k1+1)
//!                          / ( tf(t,d) + k1 · (1 - b + b · dl(d) / avgdl) )
//! ```
//! Defaults: `k1 = 1.5`, `b = 0.75` (bm25s default).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const DEFAULT_K1: f32 = 1.5;
const DEFAULT_B: f32 = 0.75;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bm25Index {
  /// token → term id
  pub vocab: HashMap<String, u32>,
  /// idf[term_id]
  pub idf: Vec<f32>,
  /// average document length
  pub avgdl: f32,
  /// doc_len[doc_id] in tokens
  pub doc_len: Vec<u32>,
  /// postings[term_id] = sorted-by-doc list of (doc_id, term_freq)
  pub postings: Vec<Vec<(u32, u32)>>,
  pub k1: f32,
  pub b: f32,
}

impl Bm25Index {
  /// Build an index from already-tokenized documents.
  ///
  /// `docs[i]` is the bag-of-tokens for document `i`. Token order doesn't
  /// matter — only frequencies are used.
  pub fn build(docs: Vec<Vec<String>>) -> Self {
    Self::build_with_params(docs, DEFAULT_K1, DEFAULT_B)
  }

  pub fn build_with_params(docs: Vec<Vec<String>>, k1: f32, b: f32) -> Self {
    let n_docs = docs.len();
    let doc_len: Vec<u32> = docs.iter().map(|d| d.len() as u32).collect();
    let avgdl = if n_docs == 0 {
      0.0
    } else {
      doc_len.iter().map(|&l| l as f64).sum::<f64>() as f32 / n_docs as f32
    };

    // Assign term ids in order of first appearance.
    let mut vocab: HashMap<String, u32> = HashMap::new();
    // postings[term_id] = Vec<(doc_id, tf)>; built incrementally in doc order
    // so the resulting postings lists are naturally sorted by doc_id.
    let mut postings: Vec<Vec<(u32, u32)>> = Vec::new();

    for (doc_id, tokens) in docs.iter().enumerate() {
      // Per-doc token frequencies. Use a small HashMap; could swap for
      // a vec-of-(term_id, count) sort+dedupe if profiling shows hot.
      let mut tf: HashMap<u32, u32> = HashMap::with_capacity(tokens.len().min(64));
      for tok in tokens {
        let term_id = if let Some(&id) = vocab.get(tok) {
          id
        } else {
          let id = vocab.len() as u32;
          vocab.insert(tok.clone(), id);
          postings.push(Vec::new());
          id
        };
        *tf.entry(term_id).or_insert(0) += 1;
      }
      for (term_id, count) in tf {
        postings[term_id as usize].push((doc_id as u32, count));
      }
    }

    // df[t] = number of distinct docs containing t = postings[t].len()
    let n_docs_f = n_docs as f32;
    let idf: Vec<f32> = postings
      .iter()
      .map(|p| {
        let df = p.len() as f32;
        ((n_docs_f - df + 0.5) / (df + 0.5) + 1.0).ln()
      })
      .collect();

    Self {
      vocab,
      idf,
      avgdl,
      doc_len,
      postings,
      k1,
      b,
    }
  }

  pub fn doc_count(&self) -> usize {
    self.doc_len.len()
  }

  /// Score every document against `query_tokens`.
  ///
  /// `query_tokens` is the output of `tokens::tokenize(query)`. Duplicates
  /// matter — repeated query tokens contribute multiple times (matches the
  /// standard BM25 sum-over-query-terms semantics that bm25s implements).
  ///
  /// `mask`, if provided, is a `Vec<bool>` of length `doc_count()`. Scores
  /// for `false` entries are zeroed *after* scoring, preserving the IDF
  /// normalization across the full corpus.
  pub fn get_scores(&self, query_tokens: &[String], mask: Option<&[bool]>) -> Vec<f32> {
    let n = self.doc_count();
    let mut scores = vec![0.0f32; n];
    if n == 0 || self.avgdl == 0.0 {
      return scores;
    }

    let k1 = self.k1;
    let b = self.b;
    let avgdl = self.avgdl;

    for tok in query_tokens {
      let Some(&term_id) = self.vocab.get(tok) else {
        continue; // OOV — contributes nothing
      };
      let idf = self.idf[term_id as usize];
      // Skip negative IDF (common in BM25Okapi when df > N/2). bm25s's
      // lucene variant uses log(1 + ...) which is always non-negative,
      // but guard anyway in case future tweaks reintroduce negatives.
      if idf <= 0.0 {
        continue;
      }
      for &(doc_id, tf) in &self.postings[term_id as usize] {
        let dl = self.doc_len[doc_id as usize] as f32;
        let tf = tf as f32;
        let denom = tf + k1 * (1.0 - b + b * dl / avgdl);
        scores[doc_id as usize] += idf * tf * (k1 + 1.0) / denom;
      }
    }

    if let Some(mask) = mask {
      debug_assert_eq!(mask.len(), n, "mask length must match doc_count");
      for (s, &keep) in scores.iter_mut().zip(mask.iter()) {
        if !keep {
          *s = 0.0;
        }
      }
    }

    scores
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::search::tokens::tokenize;

  fn s(x: &str) -> String {
    x.to_string()
  }

  #[test]
  fn empty_corpus_returns_empty_scores() {
    let idx = Bm25Index::build(vec![]);
    assert_eq!(idx.doc_count(), 0);
    assert!(idx.get_scores(&[s("anything")], None).is_empty());
  }

  #[test]
  fn single_doc_matches_query_term() {
    let idx = Bm25Index::build(vec![vec![s("hello"), s("world")]]);
    let scores = idx.get_scores(&[s("hello")], None);
    assert_eq!(scores.len(), 1);
    // With N=1 and df=1: IDF = ln(1 + (1 - 1 + 0.5) / (1 + 0.5)) = ln(1 + 1/3) ≈ 0.2877.
    // dl = 2, avgdl = 2 → length norm = 1.0; tf = 1, k1 = 1.5
    // score = 0.2877 * 1 * 2.5 / (1 + 1.5 * 1.0) = 0.2877 * 2.5 / 2.5 = 0.2877
    assert!((scores[0] - 0.28768207).abs() < 1e-5);
  }

  #[test]
  fn oov_query_returns_zeros() {
    let idx = Bm25Index::build(vec![vec![s("hello")]]);
    let scores = idx.get_scores(&[s("nope")], None);
    assert_eq!(scores, vec![0.0]);
  }

  #[test]
  fn doc_with_more_term_occurrences_scores_higher() {
    let idx = Bm25Index::build(vec![
      vec![s("foo"), s("bar")],
      vec![s("foo"), s("foo"), s("foo"), s("bar")],
    ]);
    let scores = idx.get_scores(&[s("foo")], None);
    assert!(
      scores[1] > scores[0],
      "more 'foo' occurrences should score higher"
    );
  }

  #[test]
  fn shorter_doc_scores_higher_at_equal_tf() {
    // BM25 length normalization: between two docs each containing one "foo",
    // the shorter doc should rank higher.
    let idx = Bm25Index::build(vec![
      vec![s("foo")],                                 // dl=1
      vec![s("foo"), s("a"), s("b"), s("c"), s("d")], // dl=5
    ]);
    let scores = idx.get_scores(&[s("foo")], None);
    assert!(scores[0] > scores[1], "shorter doc must score higher");
  }

  #[test]
  fn rare_term_scores_higher_than_common_term() {
    let idx = Bm25Index::build(vec![
      vec![s("common"), s("rare")],
      vec![s("common")],
      vec![s("common")],
      vec![s("common")],
    ]);
    let common_score = idx.get_scores(&[s("common")], None)[0];
    let rare_score = idx.get_scores(&[s("rare")], None)[0];
    assert!(
      rare_score > common_score,
      "rare term IDF must dominate common term IDF (rare={rare_score}, common={common_score})"
    );
  }

  #[test]
  fn mask_zeros_filtered_docs_without_renormalizing() {
    let idx = Bm25Index::build(vec![
      vec![s("foo"), s("bar")],
      vec![s("foo")],
      vec![s("foo"), s("foo")],
    ]);
    let unmasked = idx.get_scores(&[s("foo")], None);
    let mask = vec![true, false, true];
    let masked = idx.get_scores(&[s("foo")], Some(&mask));
    // Kept docs preserve their *original* scores (post-filter weight, not slice).
    assert_eq!(masked[0], unmasked[0]);
    assert_eq!(masked[1], 0.0);
    assert_eq!(masked[2], unmasked[2]);
  }

  #[test]
  fn duplicate_query_terms_compound_score() {
    let idx = Bm25Index::build(vec![vec![s("foo"), s("bar")]]);
    let single = idx.get_scores(&[s("foo")], None)[0];
    let doubled = idx.get_scores(&[s("foo"), s("foo")], None)[0];
    // Sum-over-query-terms: doubling the query term doubles the score.
    assert!((doubled - 2.0 * single).abs() < 1e-5);
  }

  #[test]
  fn end_to_end_with_real_tokenizer() {
    // Build an index from text using the actual tokenize() pipeline,
    // verify a recognizably-relevant document ranks first.
    let docs = vec![
      tokenize("class HandlerStack: pass"),
      tokenize("def parse_json(s): return json.loads(s)"),
      tokenize("class XMLParser: def parse(self, source): pass"),
    ];
    let idx = Bm25Index::build(docs);

    let query = tokenize("XMLParser");
    let scores = idx.get_scores(&query, None);

    let best = scores
      .iter()
      .enumerate()
      .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
      .unwrap()
      .0;
    assert_eq!(best, 2, "doc with 'XMLParser' should rank first");
  }
}
