//! Static-embedding model loader (model2vec / potion-code-16M).
//!
//! `Embedder::open(model_dir)` mmaps `model.safetensors`, loads `tokenizer.json`
//! into [`WordPiece`], and exposes `encode_one(text)` which
//! returns a normalized `[f32; DIM]` for a single string.
//!
//! The model is a "static" embedder: no neural-net inference, just a `vocab × dim`
//! matrix. Encoding is `tokenize → first MAX_EMBED_TOKENS ids → unweighted
//! mean → L2-normalize` (see [`Embedder::encode_one`] for why it is not quite
//! model2vec's own `encode`). Cost per call is dominated by tokenization
//! (~10–100 µs depending on string length); the embedding lookup itself is
//! essentially free.
//!
//! The matrix ships as either `f32` or `f16` depending on the model (some
//! model2vec exports ship `f16` to halve download/cache size). `f16` tensors
//! are decoded to an owned `Vec<f32>` once at open time so every consumer
//! downstream of `Embedder` (`row`, `all_rows`, `cosine_topk`) only ever deals
//! with `f32`.

use crate::search::wordpiece::WordPiece;
use half::f16;
use memmap2::Mmap;
use safetensors::{Dtype, SafeTensors};
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

/// Output dimension of `potion-code-16M`. Embedded as a const so callers can
/// stack-allocate result buffers.
pub const DIM: usize = 256;

/// Tensor name inside `model.safetensors`. model2vec's convention.
const EMBEDDINGS_TENSOR: &str = "embeddings";

/// model2vec's token-id → embedding-row table. The identity unless a model's
/// vocabulary was quantized so that several tokens share a row.
const MAPPING_TENSOR: &str = "mapping";

/// Tokens per text that [`Embedder::encode_one`] averages: the head of
/// anything longer.
///
/// potion-code-16M's `tokenizer.json` ships the same 512 as its truncation
/// setting, but the window decides ranking, so it lives here rather than in a
/// downloaded file another model could ship without. Measured on the five
/// pinned eval sets (150 queries): 384–768 is a plateau, while 256 and no
/// limit at all both rank worse (see wiki/search.md).
pub const MAX_EMBED_TOKENS: usize = 512;

/// How [`Embedder::encode_one`] pools token vectors: an unweighted mean.
/// Recorded in the index meta with [`MAX_EMBED_TOKENS`], so changing either
/// forces a rebuild instead of mixing vectors made two ways.
pub const POOLING: &str = "mean";

/// Backing memory for `Embedder::embeddings_ptr`. `F32` tensors are read
/// zero-copy straight out of the mmap when 4-byte aligned, and decoded into
/// an owned buffer (`decode_f32_le`) when a malformed file misaligns them;
/// `F16` tensors are always decoded once into an owned buffer
/// (`decode_f16_le`) — so the rest of the codebase never has to think about
/// the on-disk dtype.
///
/// Neither variant's payload is read directly — each just needs to stay alive
/// (and be dropped) for as long as `embeddings_ptr` points into it.
#[allow(dead_code)]
enum Backing {
  Mmap(Arc<Mmap>),
  Owned(Vec<f32>),
}

pub struct Embedder {
  /// Keeps whichever memory `embeddings_ptr` points into alive for the
  /// lifetime of the embedder.
  _backing: Backing,
  /// vocab_size × DIM rows of f32, row-major, borrowed from `_backing`.
  /// Stored as a raw pointer + length so `Embedder` can be `Send + Sync`.
  embeddings_ptr: *const f32,
  vocab_size: usize,
  /// Row for each token id, when the model has a non-identity `mapping`.
  mapping: Option<Vec<u32>>,
  tokenizer: WordPiece,
}

// SAFETY: both `Backing` variants are immutable after `Embedder::open` returns
// (mmap'd file bytes, or a `Vec<f32>` nothing else holds a mutable reference
// to), so shared reads from many threads are safe. The other fields are plain
// data read through `&self` — the raw pointer is the only reason these impls
// are needed at all.
unsafe impl Send for Embedder {}
unsafe impl Sync for Embedder {}

impl Embedder {
  /// Open the cached model files from `model_dir`. Expects:
  /// - `<model_dir>/model.safetensors` containing a single `f32` or `f16`
  ///   tensor named `embeddings` with shape `[vocab_size, DIM]`.
  /// - `<model_dir>/tokenizer.json` describing the BERT WordPiece pipeline
  ///   [`WordPiece`] implements.
  pub fn open(model_dir: &Path) -> io::Result<Self> {
    let safetensors_path = model_dir.join("model.safetensors");
    let tokenizer_path = model_dir.join("tokenizer.json");

    let file = File::open(&safetensors_path)?;
    let mmap = unsafe { Mmap::map(&file) }?;
    let mmap = Arc::new(mmap);

    // Parse the safetensors header against the same byte slice the matrix
    // lives in, then extract the embeddings tensor.
    let st = SafeTensors::deserialize(&mmap[..])
      .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("safetensors: {e}")))?;
    let names: Vec<&str> = st.names().into_iter().collect();
    let tensor = st.tensor(EMBEDDINGS_TENSOR).map_err(|e| {
      io::Error::new(
        io::ErrorKind::InvalidData,
        format!("safetensors missing '{EMBEDDINGS_TENSOR}' tensor (have: {names:?}): {e}"),
      )
    })?;
    let shape = tensor.shape();
    if shape.len() != 2 || shape[1] != DIM {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("expected shape [V, {DIM}], got {shape:?}"),
      ));
    }
    let vocab_size = shape[0];
    let data = tensor.data();

    let (backing, ptr) = match tensor.dtype() {
      Dtype::F32 => {
        if data.len() != vocab_size * DIM * 4 {
          return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
              "tensor data length {} != {} = vocab_size * DIM * 4",
              data.len(),
              vocab_size * DIM * 4
            ),
          ));
        }
        // mmap returns page-aligned pointers and the safetensors
        // header pads the data offset to a multiple of 8, so this
        // tensor is normally 4-byte aligned — but the file is
        // downloaded external input, and a preceding odd-length
        // tensor in a malformed file can misalign it. Reading
        // through a misaligned *const f32 is UB, so check at
        // runtime and fall back to an owned, aligned copy (same
        // shape as the F16 branch) instead of trusting a
        // debug-only assertion that vanishes in release builds.
        if (data.as_ptr() as usize).is_multiple_of(std::mem::align_of::<f32>()) {
          let ptr = data.as_ptr() as *const f32;
          (Backing::Mmap(Arc::clone(&mmap)), ptr)
        } else {
          let owned = decode_f32_le(data);
          let ptr = owned.as_ptr();
          (Backing::Owned(owned), ptr)
        }
      }
      Dtype::F16 => {
        if data.len() != vocab_size * DIM * 2 {
          return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
              "tensor data length {} != {} = vocab_size * DIM * 2",
              data.len(),
              vocab_size * DIM * 2
            ),
          ));
        }
        let owned = decode_f16_le(data);
        let ptr = owned.as_ptr();
        (Backing::Owned(owned), ptr)
      }
      other => {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          format!("expected F32 or F16 embeddings, got {other:?}"),
        ));
      }
    };

    let mapping = read_mapping(&st, vocab_size)?;

    // `WordPiece` takes the vocabulary and added tokens from the file and
    // ignores its truncation and padding: `encode_one` applies the window.
    let tokenizer = WordPiece::load(&tokenizer_path)?;
    let mapping = cover_every_token(mapping, vocab_size, tokenizer.id_count())?;

    Ok(Self {
      _backing: backing,
      embeddings_ptr: ptr,
      vocab_size,
      mapping,
      tokenizer,
    })
  }

  #[allow(dead_code)] // used by network-gated tests
  pub fn vocab_size(&self) -> usize {
    self.vocab_size
  }

  /// All embedding rows as one big slice. Read-only; backed by `_backing`.
  fn all_rows(&self) -> &[f32] {
    // SAFETY: `embeddings_ptr` is valid for `vocab_size * DIM` f32s for the
    // lifetime of `self._backing`.
    unsafe { std::slice::from_raw_parts(self.embeddings_ptr, self.vocab_size * DIM) }
  }

  /// Look up one embedding row. An index past the matrix is clamped to the
  /// last row; the tokenizer never produces one for a matching model.
  fn row(&self, index: u32) -> &[f32] {
    let id = (index as usize).min(self.vocab_size.saturating_sub(1));
    let start = id * DIM;
    &self.all_rows()[start..start + DIM]
  }

  /// The embedding row for a token: through `mapping` when the model has
  /// one, else the token id itself.
  fn token_row(&self, token_id: u32) -> &[f32] {
    let index = match &self.mapping {
      Some(m) => m.get(token_id as usize).copied().unwrap_or(token_id),
      None => token_id,
    };
    self.row(index)
  }

  /// Encode one string into a normalized `[f32; DIM]`: the unweighted mean of
  /// the rows of its first [`MAX_EMBED_TOKENS`] tokens, L2-normalized. Empty
  /// input, or input with no tokens, returns the zero vector.
  ///
  /// This departs from model2vec's own `encode` on purpose. model2vec scales
  /// each row by the model's per-token `weights` tensor (punctuation near
  /// 0.01, identifiers near 1) and drops `[UNK]`. Weighting measured worse
  /// here: on the five pinned eval sets it took dense-only MRR from 0.152 to
  /// 0.123, and hybrid recall@5 from 37 to 35 of 150 (see wiki/search.md).
  pub fn encode_one(&self, text: &str) -> [f32; DIM] {
    let mut out = [0.0f32; DIM];
    if text.is_empty() {
      return out;
    }

    let ids = self.tokenizer.encode(text, MAX_EMBED_TOKENS);
    if ids.is_empty() {
      return out;
    }

    // Sum embeddings.
    for &id in &ids {
      let row = self.token_row(id);
      for i in 0..DIM {
        out[i] += row[i];
      }
    }
    // Mean-pool.
    let inv_n = 1.0 / ids.len() as f32;
    for v in &mut out {
      *v *= inv_n;
    }
    // L2-normalize.
    let norm: f32 = out.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
      let inv = 1.0 / norm;
      for v in &mut out {
        *v *= inv;
      }
    }
    out
  }
}

/// A `mapping` entry as a row index, or `None` if it cannot be one. Narrowing
/// to `u32` first matters on 32-bit targets, where a large `i64` would wrap
/// to a small `usize` and then pass the bounds check as row 0.
fn mapping_row(index: i64, rows: usize) -> Option<u32> {
  let row = u32::try_from(index).ok()?;
  ((row as usize) < rows).then_some(row)
}

/// Read model2vec's `mapping` tensor, or `None` when the model has none. An
/// entry outside the `rows` embedding rows is an error.
fn read_mapping(st: &SafeTensors, rows: usize) -> io::Result<Option<Vec<u32>>> {
  let invalid = |msg: String| io::Error::new(io::ErrorKind::InvalidData, msg);
  let Ok(tensor) = st.tensor(MAPPING_TENSOR) else {
    return Ok(None);
  };
  let width = match tensor.dtype() {
    Dtype::I64 => 8,
    Dtype::I32 => 4,
    other => {
      return Err(invalid(format!(
        "expected an I64 or I32 `mapping`, got {other:?}"
      )))
    }
  };
  let shape = tensor.shape();
  if shape.len() != 1 || tensor.data().len() != shape[0] * width {
    return Err(invalid(format!("`mapping` has shape {shape:?}")));
  }
  let mut table = Vec::with_capacity(shape[0]);
  for bytes in tensor.data().chunks_exact(width) {
    let index = match width {
      8 => i64::from_le_bytes(bytes.try_into().expect("8 bytes")),
      _ => i32::from_le_bytes(bytes.try_into().expect("4 bytes")) as i64,
    };
    let Some(row) = mapping_row(index, rows) else {
      return Err(invalid(format!(
        "`mapping` sends a token to row {index}, outside the {rows} embedding rows"
      )));
    };
    table.push(row);
  }
  Ok(Some(table))
}

/// Check that every id the tokenizer can emit (`ids` of them) has a row:
/// through `mapping` when the model has one, directly otherwise. A short
/// table would send the ids past its end to whichever row shares their
/// number, where model2vec fails on such a model instead. An identity
/// mapping is dropped once checked, since it only adds an indirection.
fn cover_every_token(
  mapping: Option<Vec<u32>>,
  rows: usize,
  ids: usize,
) -> io::Result<Option<Vec<u32>>> {
  let invalid = |msg: String| io::Error::new(io::ErrorKind::InvalidData, msg);
  match mapping {
    Some(table) if table.len() < ids => Err(invalid(format!(
      "`mapping` has {} entries, but the tokenizer emits ids up to {}",
      table.len(),
      ids - 1
    ))),
    Some(table) => {
      let identity = table.iter().enumerate().all(|(i, &row)| row as usize == i);
      Ok((!identity).then_some(table))
    }
    None if rows < ids => Err(invalid(format!(
      "{rows} embedding rows, but the tokenizer emits ids up to {}",
      ids - 1
    ))),
    None => Ok(None),
  }
}

/// Decode a little-endian `f32` byte buffer, one value per 4-byte group —
/// the aligned-copy fallback for a misaligned F32 tensor. The mmap fast
/// path reads the same little-endian layout in place; keep the two in sync.
fn decode_f32_le(bytes: &[u8]) -> Vec<f32> {
  bytes
    .chunks_exact(4)
    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
    .collect()
}

/// Decode a little-endian `f16` (IEEE-754 binary16) byte buffer into `f32`
/// values, one per 2-byte pair. Callers validate `bytes.len()` is even (and
/// matches the expected `vocab_size * DIM * 2`) before calling this.
fn decode_f16_le(bytes: &[u8]) -> Vec<f32> {
  bytes
    .chunks_exact(2)
    .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
    .collect()
}

// ─────────────────────────────────────────────────────────────────────────
// Brute-force cosine top-k over a chunk-embedding matrix.
// ─────────────────────────────────────────────────────────────────────────

/// Threshold above which we parallelise the scan via rayon. Below this many
/// rows the per-task overhead exceeds the win.
const PAR_THRESHOLD: usize = 4096;

/// Score every row of `embeddings` against `query` and return the top-k by
/// cosine similarity (descending).
///
/// `embeddings` is the row-major `n × DIM` chunk-embedding matrix produced
/// at index time. Both `query` and every row are assumed to be L2-normalized
/// (which `Embedder::encode_one` guarantees), so cosine reduces to a dot
/// product.
///
/// `mask`, if provided, is a `Vec<bool>` of length `n`. Rows where the mask
/// is `false` are scored as `-INFINITY` and never appear in the result.
/// This matches `bm25.get_scores`'s post-filter-weight semantics — we drop
/// them entirely rather than zeroing, since 0 may legitimately rank.
///
/// Returns up to `k` `(row_id, score)` pairs sorted by score descending.
pub fn cosine_topk(
  query: &[f32; DIM],
  embeddings: &[f32],
  mask: Option<&[bool]>,
  k: usize,
) -> Vec<(u32, f32)> {
  use rayon::prelude::*;

  let n = embeddings.len() / DIM;
  debug_assert_eq!(
    embeddings.len() % DIM,
    0,
    "embeddings length not a multiple of DIM"
  );
  if let Some(m) = mask {
    debug_assert_eq!(m.len(), n, "mask length must equal row count");
  }
  if n == 0 || k == 0 {
    return Vec::new();
  }

  // Pre-load the query into 32 × f32x8 SIMD lanes so each row's dot product
  // does no extra work to splat the query.
  let q_lanes = load_query_lanes(query);

  // Score every row. Parallel for big matrices, single-threaded otherwise.
  let scores: Vec<f32> = if n >= PAR_THRESHOLD {
    (0..n)
      .into_par_iter()
      .with_min_len(256)
      .map(|i| score_row(i, embeddings, &q_lanes, mask))
      .collect()
  } else {
    (0..n)
      .map(|i| score_row(i, embeddings, &q_lanes, mask))
      .collect()
  };

  // Top-k via partial sort. For small k a min-heap is theoretically faster
  // (O(n log k) vs O(n log n)), but n is typically ~10k and k ≤ 50, so a
  // simple `select_nth_unstable_by` followed by sorting the prefix is
  // simpler and cache-friendly enough.
  let mut idx: Vec<u32> = (0..n as u32).collect();
  let take = k.min(n);
  idx.select_nth_unstable_by(take - 1, |&a, &b| {
    scores[b as usize]
      .partial_cmp(&scores[a as usize])
      .unwrap_or(std::cmp::Ordering::Equal)
  });
  let mut top: Vec<u32> = idx.into_iter().take(take).collect();
  top.sort_by(|&a, &b| {
    scores[b as usize]
      .partial_cmp(&scores[a as usize])
      .unwrap_or(std::cmp::Ordering::Equal)
  });

  top
    .into_iter()
    .filter_map(|i| {
      let s = scores[i as usize];
      // A row past the size the model handles is stored non-finite on
      // purpose, which keeps it reachable lexically and never densely.
      if s.is_finite() {
        Some((i, s))
      } else {
        None
      }
    })
    .collect()
}

#[inline]
fn score_row(
  i: usize,
  embeddings: &[f32],
  q_lanes: &[wide::f32x8; DIM / 8],
  mask: Option<&[bool]>,
) -> f32 {
  if let Some(m) = mask {
    if !m[i] {
      return f32::NEG_INFINITY;
    }
  }
  let row = &embeddings[i * DIM..(i + 1) * DIM];
  let score = dot_simd(q_lanes, row);
  // NaN participates in no ordering: `partial_cmp` answers `None` against
  // every value, and the caller reads that as a tie. A row left comparable
  // to nothing can be partitioned into the top-k window and then dropped by
  // the `is_finite` filter, returning a pool shorter than `k`. Demoting the
  // sentinel here is what makes the comparison total.
  if score.is_finite() {
    score
  } else {
    f32::NEG_INFINITY
  }
}

#[inline]
fn load_query_lanes(query: &[f32; DIM]) -> [wide::f32x8; DIM / 8] {
  let mut out = [wide::f32x8::splat(0.0); DIM / 8];
  for (i, lane) in out.iter_mut().enumerate() {
    let chunk: [f32; 8] = query[i * 8..(i + 1) * 8].try_into().unwrap();
    *lane = wide::f32x8::from(chunk);
  }
  out
}

/// SIMD dot product of a query (pre-loaded into 32 lanes) and a row slice.
/// Both operands are L2-normalized, so the dot product equals cosine.
#[inline]
fn dot_simd(q_lanes: &[wide::f32x8; DIM / 8], row: &[f32]) -> f32 {
  debug_assert_eq!(row.len(), DIM);
  let mut acc = wide::f32x8::splat(0.0);
  for (i, q) in q_lanes.iter().enumerate() {
    let chunk: [f32; 8] = row[i * 8..(i + 1) * 8].try_into().unwrap();
    let r = wide::f32x8::from(chunk);
    acc += *q * r;
  }
  // Horizontal sum.
  let arr: [f32; 8] = acc.into();
  arr.iter().sum()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::search::download::{ensure_model, ModelInfo};

  fn ensure_real_model() -> std::path::PathBuf {
    // Download once into a per-test cache; subsequent runs reuse the cache
    // and the SHA-256 manifest verification fast-paths.
    let info = ModelInfo::potion_code_16m();
    ensure_model(&info).expect("model download failed; see network-security wiki")
  }

  /// A mapping entry must become a `u32` before its row is range-checked: on
  /// a 32-bit host `i64::from(u32::MAX) + 1` would cast to row 0.
  #[test]
  fn mapping_rejects_rows_outside_u32() {
    assert_eq!(mapping_row(3, 4), Some(3));
    assert_eq!(mapping_row(4, 4), None);
    assert_eq!(mapping_row(-1, usize::MAX), None);
    assert_eq!(mapping_row(i64::from(u32::MAX) + 1, usize::MAX), None);
  }

  #[test]
  #[ignore]
  fn network_loads_potion_model() {
    let dir = ensure_real_model();
    let emb = Embedder::open(&dir).expect("Embedder::open failed");
    assert!(emb.vocab_size() > 1000, "vocab implausibly small");
  }

  #[test]
  #[ignore]
  fn network_encodes_to_unit_vector() {
    let emb = Embedder::open(&ensure_real_model()).unwrap();

    let v = emb.encode_one("def parse_json(s): return json.loads(s)");
    // Every component should be finite.
    for x in v.iter() {
      assert!(x.is_finite(), "non-finite component: {x}");
    }
    // L2-norm should be ~1 (we just normalized).
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-5, "norm = {norm}, expected ≈ 1.0");
  }

  #[test]
  #[ignore]
  fn network_similar_strings_have_high_cosine() {
    // Sanity: two semantically-similar code snippets should be closer than
    // unrelated ones. Doesn't pin to specific scores — just enforces ordering.
    let emb = Embedder::open(&ensure_real_model()).unwrap();
    let a = emb.encode_one("def add(a, b): return a + b");
    let b = emb.encode_one("def sum(x, y): return x + y");
    let c = emb.encode_one("class HttpServer: def listen(self, port): pass");

    let cos =
      |u: &[f32], v: &[f32]| -> f32 { u.iter().zip(v.iter()).map(|(x, y)| x * y).sum::<f32>() };
    let ab = cos(&a, &b);
    let ac = cos(&a, &c);
    assert!(
      ab > ac,
      "expected related code (cos {ab}) > unrelated code (cos {ac})"
    );
  }

  // ── decode_f16_le (pure unit test, no network) ──────────────────────────

  #[test]
  fn decode_f16_le_matches_known_bit_patterns() {
    // 0x3C00 = 1.0, 0x0000 = 0.0, 0xC000 = -2.0 (IEEE-754 binary16).
    let bytes = [0x00, 0x3C, 0x00, 0x00, 0x00, 0xC0];
    let decoded = decode_f16_le(&bytes);
    assert_eq!(decoded, vec![1.0f32, 0.0, -2.0]);
  }

  #[test]
  fn decode_f16_le_handles_ieee754_edge_cases() {
    // The `half` crate gets these right; the point is that the *contract*
    // is pinned, so a hand-rolled replacement can't quietly change it.
    // A NaN or infinity smuggled into a vocab row poisons every cosine
    // comparison it takes part in, and silently — hence the coverage.
    let case = |lo: u8, hi: u8| decode_f16_le(&[lo, hi])[0];

    // 0x0001 — smallest positive subnormal, exactly 2^-24.
    assert_eq!(case(0x01, 0x00), 2f32.powi(-24));
    // 0x03FF — largest subnormal, just under 2^-14.
    assert_eq!(case(0xFF, 0x03), 1023.0 * 2f32.powi(-24));
    // 0x7C00 / 0xFC00 — infinities keep their sign.
    assert_eq!(case(0x00, 0x7C), f32::INFINITY);
    assert_eq!(case(0x00, 0xFC), f32::NEG_INFINITY);
    // 0x7E00 — quiet NaN stays NaN rather than decoding to a number.
    assert!(case(0x00, 0x7E).is_nan());
    // 0x8000 — negative zero keeps its sign bit (`-0.0 == 0.0`, so this
    // has to be asserted on the bits, not the value).
    let neg_zero = case(0x00, 0x80);
    assert_eq!(neg_zero, 0.0);
    assert!(neg_zero.is_sign_negative(), "0x8000 must decode to -0.0");
    // 0x7BFF — largest finite binary16, 65504.
    assert_eq!(case(0xFF, 0x7B), 65504.0);
  }

  // ── Embedder::open dtype rejection (no network, no model needed) ───────

  /// Minimal safetensors file: 8-byte LE header length, JSON header, data.
  fn safetensors_bytes(dtype: &str, rows: usize, elem_bytes: usize) -> Vec<u8> {
    let len = rows * DIM * elem_bytes;
    let header = format!(
      r#"{{"embeddings":{{"dtype":"{dtype}","shape":[{rows},{DIM}],"data_offsets":[0,{len}]}}}}"#
    );
    let mut out = Vec::new();
    out.extend_from_slice(&(header.len() as u64).to_le_bytes());
    out.extend_from_slice(header.as_bytes());
    out.resize(out.len() + len, 0);
    out
  }

  #[test]
  fn open_rejects_an_unsupported_embedding_dtype() {
    // The dtype arm runs before the tokenizer is loaded, so this needs
    // neither tokenizer.json nor a downloaded model.
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
      dir.path().join("model.safetensors"),
      safetensors_bytes("I64", 2, 8),
    )
    .expect("write fixture");

    let err = match Embedder::open(dir.path()) {
      Err(e) => e,
      Ok(_) => panic!("I64 embeddings must be rejected"),
    };
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(
      err.to_string().contains("expected F32 or F16 embeddings"),
      "unexpected rejection message: {err}"
    );
  }

  // ── encode_one: window, mapping, weights (tiny fixture, no network) ────

  /// A safetensors file holding `tensors` as (name, dtype, shape, bytes).
  fn safetensors_file(tensors: &[(&str, &str, Vec<usize>, Vec<u8>)]) -> Vec<u8> {
    let mut header = String::from("{");
    let mut data = Vec::new();
    for (i, (name, dtype, shape, bytes)) in tensors.iter().enumerate() {
      let start = data.len();
      data.extend_from_slice(bytes);
      if i > 0 {
        header.push(',');
      }
      header.push_str(&format!(
        r#""{name}":{{"dtype":"{dtype}","shape":{shape:?},"data_offsets":[{start},{}]}}"#,
        data.len()
      ));
    }
    header.push('}');
    while header.len() % 8 != 0 {
      header.push(' ');
    }
    let mut out = (header.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&data);
    out
  }

  /// `tokenizer.json` shaped like potion-code-16M's, over four tokens.
  /// `truncation` is the file's own window setting.
  fn tokenizer_json(truncation: Option<usize>) -> String {
    let truncation = match truncation {
      Some(n) => {
        format!(r#"{{"direction":"Right","max_length":{n},"strategy":"LongestFirst","stride":0}}"#)
      }
      None => "null".to_string(),
    };
    format!(
      r###"{{"version":"1.0","truncation":{truncation},"padding":null,
      "added_tokens":[
        {{"id":0,"content":"[PAD]","single_word":true,"lstrip":true,"rstrip":true,"normalized":true,"special":true}},
        {{"id":1,"content":"[UNK]","single_word":false,"lstrip":false,"rstrip":false,"normalized":false,"special":true}}],
      "normalizer":{{"type":"BertNormalizer","clean_text":true,"handle_chinese_chars":true,"strip_accents":null,"lowercase":true}},
      "pre_tokenizer":{{"type":"BertPreTokenizer"}},
      "post_processor":null,
      "decoder":{{"type":"WordPiece","prefix":"##","cleanup":true}},
      "model":{{"type":"WordPiece","unk_token":"[UNK]","continuing_subword_prefix":"##",
        "max_input_chars_per_word":100,"vocab":{{"[PAD]":0,"[UNK]":1,"a":2,"b":3}}}}}}"###
    )
  }

  /// A model directory for the tokens `[PAD]`, `[UNK]`, `a`, `b` (ids 0–3)
  /// with `rows` embedding rows, row 2 being e0 and row 3 e1, plus
  /// model2vec's optional `mapping` and `weights`.
  fn tiny_model(
    dir: &Path,
    rows: usize,
    mapping: Option<&[i64]>,
    weights: Option<&[f64]>,
    truncation: Option<usize>,
  ) {
    let mut matrix = vec![0f32; rows * DIM];
    for (row, dim) in [(2, 0), (3, 1)] {
      if row < rows {
        matrix[row * DIM + dim] = 1.0;
      }
    }
    let mut tensors = vec![(
      "embeddings",
      "F32",
      vec![rows, DIM],
      matrix
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect::<Vec<u8>>(),
    )];
    if let Some(m) = mapping {
      tensors.push((
        "mapping",
        "I64",
        vec![m.len()],
        m.iter().flat_map(|v| v.to_le_bytes()).collect(),
      ));
    }
    if let Some(w) = weights {
      tensors.push((
        "weights",
        "F64",
        vec![w.len()],
        w.iter().flat_map(|v| v.to_le_bytes()).collect(),
      ));
    }
    std::fs::write(dir.join("model.safetensors"), safetensors_file(&tensors)).unwrap();
    std::fs::write(dir.join("tokenizer.json"), tokenizer_json(truncation)).unwrap();
  }

  #[test]
  fn encode_averages_only_the_first_max_embed_tokens() {
    let dir = tempfile::tempdir().unwrap();
    tiny_model(dir.path(), 4, None, None, None);
    let emb = Embedder::open(dir.path()).unwrap();
    let text = format!("{}{}", "a ".repeat(MAX_EMBED_TOKENS), "b ".repeat(100));
    let v = emb.encode_one(&text);
    assert_eq!(
      (v[0], v[1]),
      (1.0, 0.0),
      "tokens past the window must not count"
    );
  }

  /// The window is ast-bro's, whatever the model's `tokenizer.json` says.
  #[test]
  fn the_window_ignores_the_tokenizer_files_truncation() {
    let dir = tempfile::tempdir().unwrap();
    tiny_model(dir.path(), 4, None, None, Some(4));
    let v = Embedder::open(dir.path()).unwrap().encode_one("a a a a b");
    assert!(
      v[1] > 0.0,
      "the fifth token is inside MAX_EMBED_TOKENS: {:?}",
      &v[..2]
    );
  }

  #[test]
  fn mapping_sends_each_token_to_its_row() {
    let dir = tempfile::tempdir().unwrap();
    tiny_model(dir.path(), 4, Some(&[0, 1, 3, 2]), None, None);
    let v = Embedder::open(dir.path()).unwrap().encode_one("a");
    assert_eq!((v[0], v[1]), (0.0, 1.0));
  }

  /// Weighting measured worse on the eval sets; see `encode_one`.
  #[test]
  fn weights_are_ignored_on_purpose() {
    let dir = tempfile::tempdir().unwrap();
    tiny_model(dir.path(), 4, None, Some(&[0.0, 0.0, 0.0001, 1.0]), None);
    let v = Embedder::open(dir.path()).unwrap().encode_one("a b");
    let half = std::f32::consts::FRAC_1_SQRT_2;
    assert!(
      (v[0] - half).abs() < 1e-6 && (v[1] - half).abs() < 1e-6,
      "a and b must count equally: {:?}",
      &v[..2]
    );
  }

  /// Every id the tokenizer can emit needs a row: a mapping that stops
  /// short would send `b` (id 3) to row 3, which the table gives `a`.
  #[test]
  fn open_rejects_a_mapping_that_misses_a_token() {
    let dir = tempfile::tempdir().unwrap();
    tiny_model(dir.path(), 4, Some(&[0, 1, 3]), None, None);
    let err = match Embedder::open(dir.path()) {
      Err(e) => e,
      Ok(_) => panic!("a mapping shorter than the vocabulary must be rejected"),
    };
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("has 3 entries"), "got: {err}");
  }

  #[test]
  fn open_rejects_fewer_rows_than_token_ids() {
    let dir = tempfile::tempdir().unwrap();
    tiny_model(dir.path(), 3, None, None, None);
    let err = match Embedder::open(dir.path()) {
      Err(e) => e,
      Ok(_) => panic!("a matrix without a row for every token must be rejected"),
    };
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("3 embedding rows"), "got: {err}");
  }

  #[test]
  fn open_rejects_a_mapping_outside_the_rows() {
    let dir = tempfile::tempdir().unwrap();
    tiny_model(dir.path(), 4, Some(&[0, 1, 9, 3]), None, None);
    let err = match Embedder::open(dir.path()) {
      Err(e) => e,
      Ok(_) => panic!("a mapping past the matrix must be rejected"),
    };
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(
      err.to_string().contains("outside the 4 embedding rows"),
      "got: {err}"
    );
  }

  // ── cosine_topk (pure unit tests, no network) ──────────────────────────

  fn unit(values: &[f32]) -> Vec<f32> {
    let mut v = values.to_vec();
    v.resize(DIM, 0.0);
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
      for x in &mut v {
        *x /= n;
      }
    }
    v
  }

  /// An unembedded row is never a dense candidate, however the pool is sized.
  #[test]
  fn a_non_finite_row_never_enters_the_top_k() {
    // A zero vector cannot serve as the sentinel: its dot product is a
    // finite 0.0, which outranks every negative similarity and so enters
    // the pool whenever the corpus is smaller than the pool — the normal
    // case for a small repository.
    let mut rows = vec![0.0f32; DIM * 3];
    rows[..DIM].fill(1.0 / (DIM as f32).sqrt()); // row 0: matches
    rows[DIM..DIM * 2].fill(-1.0 / (DIM as f32).sqrt()); // row 1: opposes
    rows[DIM * 2..].fill(f32::NAN); // row 2: never embedded
    let mut query = [0.0f32; DIM];
    query.fill(1.0 / (DIM as f32).sqrt());

    let top = cosine_topk(&query, &rows, None, 10);
    let ids: Vec<u32> = top.iter().map(|(i, _)| *i).collect();
    assert!(ids.contains(&0), "the matching row should rank");
    assert!(ids.contains(&1), "even an opposing row is a candidate");
    assert!(
      !ids.contains(&2),
      "the non-embedded row must not be a candidate"
    );
    assert_eq!(top.len(), 2, "asked for 10 of 3 rows, one is excluded");
  }

  /// The pool holds `k` embedded rows even when unembedded rows sort first.
  ///
  /// Red here means the sentinel stopped being comparable, so unembedded
  /// rows occupy window slots and the `is_finite` filter returns a short pool.
  #[test]
  fn non_finite_rows_do_not_steal_the_top_k_window() {
    // Unembedded rows come first so a partition that treats them as ties
    // has every chance to keep them, which is the case that fails when the
    // sentinel is left incomparable.
    let unit = 1.0 / (DIM as f32).sqrt();
    let mut rows = vec![f32::NAN; DIM * 6];
    for (n, scale) in [(3usize, 1.0f32), (4, 0.9), (5, 0.8)] {
      rows[n * DIM..(n + 1) * DIM].fill(unit * scale);
    }
    let mut query = [0.0f32; DIM];
    query.fill(unit);

    let top = cosine_topk(&query, &rows, None, 3);
    let ids: Vec<u32> = top.iter().map(|(i, _)| *i).collect();
    assert_eq!(ids, vec![3, 4, 5], "the three embedded rows, best first");
    assert!(top.iter().all(|(_, s)| s.is_finite()), "every score finite");
  }

  #[test]
  fn cosine_topk_empty_returns_empty() {
    let q = [0.0f32; DIM];
    assert!(cosine_topk(&q, &[], None, 5).is_empty());
  }

  #[test]
  fn cosine_topk_zero_k_returns_empty() {
    let q = [0.0f32; DIM];
    let rows = vec![0.0f32; DIM];
    assert!(cosine_topk(&q, &rows, None, 0).is_empty());
  }

  #[test]
  fn cosine_topk_orders_by_similarity() {
    // Three rows along ±axes; query along +x. Best match is row 0.
    let mut rows = Vec::new();
    rows.extend(unit(&[1.0, 0.0, 0.0])); // row 0: same direction as q
    rows.extend(unit(&[0.0, 1.0, 0.0])); // row 1: orthogonal
    rows.extend(unit(&[-1.0, 0.0, 0.0])); // row 2: opposite

    let q: [f32; DIM] = unit(&[1.0, 0.0, 0.0]).try_into().unwrap();
    let top = cosine_topk(&q, &rows, None, 3);

    assert_eq!(top.len(), 3);
    assert_eq!(top[0].0, 0);
    assert!((top[0].1 - 1.0).abs() < 1e-5);
    assert_eq!(top[1].0, 1);
    assert!(top[1].1.abs() < 1e-5); // ≈ 0
    assert_eq!(top[2].0, 2);
    assert!((top[2].1 + 1.0).abs() < 1e-5); // ≈ -1
  }

  #[test]
  fn cosine_topk_respects_k() {
    let mut rows = Vec::new();
    for i in 0..10 {
      // Each row aligns with query proportionally — row 0 best, row 9 worst.
      let mag = 1.0 - (i as f32) * 0.1;
      rows.extend(unit(&[mag, 0.1, 0.0]));
    }
    let q: [f32; DIM] = unit(&[1.0, 0.0, 0.0]).try_into().unwrap();
    let top = cosine_topk(&q, &rows, None, 3);
    assert_eq!(top.len(), 3);
    // Top-3 should be rows 0, 1, 2 in order.
    assert_eq!(top[0].0, 0);
    assert_eq!(top[1].0, 1);
    assert_eq!(top[2].0, 2);
  }

  #[test]
  fn cosine_topk_mask_excludes_filtered_rows() {
    let mut rows = Vec::new();
    rows.extend(unit(&[1.0, 0.0, 0.0])); // row 0: best
    rows.extend(unit(&[0.9, 0.1, 0.0])); // row 1: second best
    rows.extend(unit(&[0.8, 0.2, 0.0])); // row 2: third best

    let q: [f32; DIM] = unit(&[1.0, 0.0, 0.0]).try_into().unwrap();
    // Mask out the top row.
    let mask = vec![false, true, true];
    let top = cosine_topk(&q, &rows, Some(&mask), 5);
    // Should return rows 1 and 2 only — row 0 was filtered before scoring.
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].0, 1);
    assert_eq!(top[1].0, 2);
  }

  #[test]
  fn cosine_topk_handles_large_matrix() {
    // Cross the parallel threshold to exercise the rayon path.
    let n = 5000;
    let mut rows = Vec::with_capacity(n * DIM);
    for i in 0..n {
      // Make row i best at "index i" with falling similarity to q (which is row 0).
      let mut v = vec![0.0f32; 3];
      v[0] = 1.0 - (i as f32) / (n as f32);
      v[1] = (i as f32) / (n as f32);
      rows.extend(unit(&v));
    }
    let q: [f32; DIM] = unit(&[1.0, 0.0, 0.0]).try_into().unwrap();
    let top = cosine_topk(&q, &rows, None, 5);
    assert_eq!(top.len(), 5);
    // Top result should be row 0 (full alignment with q).
    assert_eq!(top[0].0, 0);
    // Scores should be monotonically non-increasing.
    for w in top.windows(2) {
      assert!(w[0].1 >= w[1].1, "scores not monotone: {:?}", top);
    }
  }

  #[test]
  #[ignore]
  fn network_empty_returns_zero_vector() {
    let emb = Embedder::open(&ensure_real_model()).unwrap();
    let v = emb.encode_one("");
    assert!(v.iter().all(|&x| x == 0.0));
  }
}
