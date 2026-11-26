//! Single unified resolver: turn a normalised import string (with
//! per-language hints) into a concrete file path.
//!
//! A project-wide suffix index lets every language share one resolver
//! instead of nine.
//!
//! The strategy:
//!  1. Build a suffix index once per graph build. For each file `a/b/c.py`
//!     the index records suffixes `c`, `b/c`, `a/b/c` → file. Python
//!     `__init__.py` also indexes the parent dir name. Java/Kotlin/Scala
//!     also index `<package>/<TypeName>` → file.
//!  2. Resolve queries by progressive lookup of the longest matching
//!     suffix; on multi-match, pick the entry sharing the most leading
//!     path components with the importer (`pick_closest`).

pub mod build;
pub mod resolve;

pub use build::{build_suffix_index, Lang, SuffixIndex};
pub use resolve::{resolve, resolve_from_import, ResolveCtx};
