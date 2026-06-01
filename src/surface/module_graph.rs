//! Resolve `mod foo;` references to actual files using the standard
//! Rust 2018 conventions.
//!
//! Rules implemented (in order, first hit wins):
//! 1. `#[path = "..."]` attribute on the `mod` declaration → that file
//!    (relative to the file containing the `mod` decl).
//! 2. Sibling file `<dir>/foo.rs`.
//! 3. Submodule directory `<dir>/foo/mod.rs`.
//! 4. Submodule directory `<dir>/foo/<foo>.rs` (rare; only used if (2)
//!    and (3) are both missing — we still try as a courtesy).
//!
//! `<dir>` for a file `foo.rs` is its parent directory; for `mod.rs` or
//! `lib.rs`/`main.rs` it is also the parent. There's no special case
//! needed because the convention is "child mods live under the
//! containing file's directory" for both forms.

use crate::surface::imports::ModRef;
use std::path::{Path, PathBuf};

/// Returns the resolved file path for a `mod foo;` reference declared in
/// `containing_file`, or `None` if no candidate exists on disk.
pub fn resolve_mod_file(containing_file: &Path, m: &ModRef) -> Option<PathBuf> {
    let parent = containing_file.parent()?;

    if let Some(rel) = &m.path_attr {
        let p = parent.join(rel);
        if p.is_file() {
            return Some(p);
        }
    }

    let candidates = [
        parent.join(format!("{}.rs", m.name)),
        parent.join(&m.name).join("mod.rs"),
        parent.join(&m.name).join(format!("{}.rs", m.name)),
    ];
    candidates.into_iter().find(|c| c.is_file())
}
