//! `ast-bro surface` — compute the true public API surface of a package.
//!
//! Where `digest` filters per-file by visibility, `surface` resolves the
//! transitive re-export graph from the package entry point, so it shows
//! exactly the symbols a downstream user can reach (and *only* those).
//!
//! - **Rust**: starts at `lib.rs`/`main.rs`, follows `pub use` and `pub mod`,
//!   handles `as` rename, glob `*`, Cargo workspaces.
//! - **Python**: starts at `__init__.py`, honours `__all__` when present
//!   and the leading-underscore convention otherwise, follows
//!   `from .x import y` / `from .x import *` into sub-packages.
//! - **Java / C# / Go / Kotlin**: visibility-filtered fallback — these
//!   languages have no real re-export concept, so `digest --no-private`
//!   IS the public surface.
//!
//! The detailed pipeline lives in the per-language modules; `mod.rs`
//! exports the public types and the dispatch entry point.

pub mod entry;
pub mod entry_point;
pub mod fallback;
pub mod imports;
pub mod manifest;
pub mod module_graph;
pub mod options;
pub mod python;
pub mod render;
pub mod rust;
pub mod scala;
pub mod typescript;

pub use entry::SurfaceEntry;
pub use entry_point::{discover, EntryPoint};
pub use options::{LangOverride, OutputMode, SurfaceError, SurfaceOptions};

use std::path::Path;

/// Public dispatch entry point. Resolves the entry point under `path`,
/// dispatches to the right per-language resolver, and returns the
/// flattened surface entries (or a structured error).
pub fn resolve_surface(
    path: &Path,
    opts: &SurfaceOptions,
) -> Result<Vec<SurfaceEntry>, SurfaceError> {
    let entry = match opts.lang_override {
        Some(l) => entry_point::discover_as(path, l)?,
        None => discover(path)?,
    };
    resolve_entry(&entry, opts)
}

/// Recursively dispatch a discovered entry point. Workspace members
/// fan out and their entries are concatenated, prefixed by crate name.
pub fn resolve_entry(
    entry: &EntryPoint,
    opts: &SurfaceOptions,
) -> Result<Vec<SurfaceEntry>, SurfaceError> {
    match entry {
        EntryPoint::RustCrate { .. } => rust::resolve(entry, opts),
        EntryPoint::RustWorkspace { members } => {
            let mut all = Vec::new();
            for m in members {
                all.extend(resolve_entry(m, opts)?);
            }
            Ok(all)
        }
        EntryPoint::PythonPackage { .. } => python::resolve(entry, opts),
        EntryPoint::TsPackage { .. } => typescript::resolve(entry, opts),
        EntryPoint::ScalaPackage { .. } => scala::resolve(entry, opts),
        EntryPoint::Fallback { .. } => fallback::resolve(entry, opts),
    }
}
