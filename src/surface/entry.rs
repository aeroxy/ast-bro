use crate::core::{DeclarationGroup, DeclarationKind};
use serde::{Serialize, Serializer};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct ReExportHop {
    #[serde(serialize_with = "_ser_path")]
    pub file: PathBuf,
    pub line: usize,
    pub module_path: String,
    pub statement: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SurfaceEntry {
    /// Fully qualified name as a downstream user would write it
    /// (e.g. `mycrate::net::Client`, `mypkg.public_fn`).
    pub qualified_path: String,
    pub kind: DeclarationKind,
    pub signature: String,
    #[serde(serialize_with = "_ser_path")]
    pub source_path: PathBuf,
    pub source_line: usize,
    /// Original identifier in the source file (pre-rename).
    /// For Rust `pub use foo::Bar as Baz;` the qualified path ends in `Baz`
    /// but `source_name` is `Bar`.
    pub source_name: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub re_export_chain: Vec<ReExportHop>,
    #[serde(skip_serializing_if = "_is_false")]
    pub via_glob: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub docs: Vec<String>,
    /// The documented block this symbol is declared in, when it is
    /// declared in one.
    ///
    /// Copied from the declaration whenever it has a group, independently
    /// of `docs`, which stays whatever the symbol documents about itself.
    /// The two answer different questions and a member can answer both,
    /// so a resolver carrying `group` only when `docs` is empty erases a
    /// block's comment from exactly the members that document themselves.
    ///
    /// Kept beside `docs` so a resolver copying one sees the other: a
    /// surface carrying only `docs` reports every member of a documented
    /// Go `const ( … )` block as undocumented (issue #46).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<DeclarationGroup>,
}

fn _is_false(b: &bool) -> bool {
    !*b
}

fn _ser_path<S: Serializer>(p: &Path, ser: S) -> Result<S::Ok, S::Error> {
    ser.serialize_str(&p.to_string_lossy())
}
