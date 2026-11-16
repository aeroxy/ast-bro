//! Phase-1 IR shared by `build` (producer) and `resolve` (consumer).
//!
//! Lives here rather than inside `build` so the two halves of the call-graph
//! pipeline can both depend on the IR without depending on each other —
//! `build` would otherwise import `resolve::run` while `resolve` imported
//! `build::FilePass`, forming an avoidable file-level cycle.

use crate::calls::graph::{CallEdge, CallKindCompat, CallTarget, CallableMeta, Confidence, Qn, TypeMeta};
use crate::core::{ImportBinding, ReceiverScope};
use std::path::{Path, PathBuf};

/// Per-file output of phase 1: the file's qns, its imports, and its raw
/// (unresolved) call-edges. Phase 2 in `resolve.rs` joins these globally.
pub struct FilePass {
    pub file: PathBuf,
    /// All qns this file defines — top-level + nested. Order matters only
    /// for stable output; pass A consumes as a set.
    pub defined: Vec<Qn>,
    /// Per-callable file/line metadata for the qns in `defined`. Same
    /// length and order. Aggregated into `CallGraph::callable_meta`.
    pub callable_locations: Vec<CallableMeta>,
    /// `local_name → module spec` from `use`/`import`/`using` statements.
    pub imports: Vec<ImportBinding>,
    /// One entry per call site in the file. `target` is `Bare` here; the
    /// resolver promotes it.
    pub raw_edges: Vec<RawEdge>,
    /// Type declarations — class/struct/interface/trait/enum/record. Carries
    /// `bases` so the implementors-reverse index can be built without a
    /// second walk. Independent of the call-edge pipeline.
    pub types: Vec<(Qn, TypeMeta)>,
    /// Namespace declarations (a Ruby `module`, a C++ `namespace`). With the
    /// types, they are the scopes a scope receiver can name.
    pub namespaces: Vec<Qn>,
}

#[derive(Debug, Clone)]
pub struct RawEdge {
    pub source: Qn,
    pub bare_name: String,
    pub receiver: Option<String>,
    pub kind: CallKindCompat,
    pub line: u32,
    pub receiver_scope: ReceiverScope,
    pub imports: Vec<crate::core::Imported>,
    pub arity: Option<usize>,
}

pub fn qn_from(rel_file: &str, parents: &[String]) -> Qn {
    let scope = parents.join("::");
    if scope.is_empty() {
        Qn::new(rel_file.to_string())
    } else {
        Qn::new(format!("{}::{}", rel_file, scope))
    }
}

pub fn file_rel(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn raw_to_edge(
    raw: RawEdge,
    target: CallTarget,
    confidence: Confidence,
    file: PathBuf,
    candidates: Vec<Qn>,
) -> CallEdge {
    CallEdge {
        source: raw.source,
        target,
        kind: raw.kind,
        line: raw.line,
        file,
        confidence,
        receiver: raw.receiver,
        candidates,
        receiver_scope: raw.receiver_scope,
        imports: raw.imports,
        arity: raw.arity,
    }
}
