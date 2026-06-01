//! Hybrid code search (BM25 + dense embeddings) over a per-repo persistent index.
//!
//! Public entry points:
//! - `Index::open(path)` / `Index::build(path)` — open or build the on-disk index
//! - `index.search(query, opts)` — hybrid retrieval + ranked top-k
//! - `index.find_related(file, line, opts)` — semantic similarity from a chunk
//!
//! The whole module is gated behind no feature flags for v1 — search is always built in.

pub mod bm25;
pub mod cache;
pub mod chunker;
pub mod cli;
pub mod download;
pub mod embed;
pub mod format;
pub mod fusion;
pub mod index;
pub mod mcp;
pub mod query;
pub mod ranking;
pub mod shared;
pub mod tokens;
