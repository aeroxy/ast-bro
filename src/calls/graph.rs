//! `CallGraph` — symbol-level call graph living inside the unified cache.
//!
//! Contrast with `DepGraph` (file-level): nodes here are *qualified names*
//! (`<repo-relative-path>::<NestedScope>::<name>`), not files. Edges carry
//! confidence so consumers can filter out fuzzy matches in CI gates.

use crate::core::{CallKind, JSON_SCHEMA_GRAPH_INDEX};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

/// Project-unique identifier for a callable. Format:
///   `<repo-relative-posix-path>::<NestedScope>::<name>`
/// e.g. `src/auth/login.rs::AuthService::authenticate`.
/// Module-scope free functions: `src/util.py::helper`.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Qn(pub String);

impl Qn {
    pub fn new<S: Into<String>>(s: S) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// Repo-relative file path component (everything before the first `::`).
    pub fn file(&self) -> &str {
        match self.0.find("::") {
            Some(i) => &self.0[..i],
            None => &self.0,
        }
    }
    /// Terminal symbol name (everything after the last `::`).
    pub fn name(&self) -> &str {
        match self.0.rfind("::") {
            Some(i) => &self.0[i + 2..],
            None => &self.0,
        }
    }
}

impl std::fmt::Display for Qn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    /// Resolved via single same-file or single-import match — high trust.
    Exact,
    /// Disambiguated via dep-graph filtering — likely-but-not-certain.
    Inferred,
    /// Multiple candidates, no disambiguation possible. Kept for grep-like
    /// fallback; off by default in `callers --include-ambiguous`.
    Ambiguous,
}

impl Confidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Exact => "Exact",
            Self::Inferred => "Inferred",
            Self::Ambiguous => "Ambiguous",
        }
    }
}

/// Resolved or unresolved callee. `Bare` survives when the resolver couldn't
/// pick a single project-internal qn; `External` means we resolved the
/// import to something outside the project (third-party crate, stdlib).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CallTarget {
    Resolved(Qn),
    External(String),
    Bare(String),
}

impl CallTarget {
    pub fn display(&self) -> String {
        match self {
            Self::Resolved(qn) => qn.to_string(),
            Self::External(s) => format!("[external] {}", s),
            Self::Bare(s) => format!("[unresolved] {}", s),
        }
    }

    /// Bare callee identifier — the qn's terminal name when resolved, or
    /// the raw string for External/Bare. Used by the incremental updater
    /// to demote a stale Resolved edge back to Bare without losing the
    /// originally observed callee text.
    pub fn name_or_raw(&self) -> String {
        match self {
            Self::Resolved(qn) => qn.name().to_string(),
            Self::External(s) | Self::Bare(s) => s.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallEdge {
    pub source: Qn,
    pub target: CallTarget,
    pub kind: CallKindCompat,
    pub line: u32,
    /// Repo-relative source file path for fast filtering / display.
    pub file: PathBuf,
    pub confidence: Confidence,
    /// Receiver as written at the call site (`obj` for `obj.method()`,
    /// `Foo` for `Foo::bar()` or `Foo.bar()` — i.e. the unit-struct value
    /// case). Lets `callers <Type>` find usages where the type appears as
    /// a receiver but no construction syntax was used.
    ///
    /// No `skip_serializing_if` — see `DepEdge::local_name` for the
    /// bincode positional-encoding rationale. Skipping a field corrupts
    /// the entire cache round-trip silently.
    #[serde(default)]
    pub receiver: Option<String>,
    /// All candidates considered when `Confidence::Ambiguous`. Empty
    /// otherwise. Same no-skip rule as `receiver`.
    #[serde(default)]
    pub candidates: Vec<Qn>,
}

// `CallEdge::target.name_or_raw()` is the only consumer surface — kept on
// `CallTarget` itself so the incremental updater can reach it without
// unwrapping the variant. No `target_name_or_bare()` shortcut: it would
// just re-export an existing one-line method.

/// Serializable mirror of `crate::core::CallKind`. We keep this separate to
/// avoid putting `Deserialize` on the public IR type and to keep the cache
/// schema stable when the IR enum grows new variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CallKindCompat {
    Call,
    Construct,
    Macro,
    Super,
    Implement,
}

impl From<CallKind> for CallKindCompat {
    fn from(k: CallKind) -> Self {
        match k {
            CallKind::Call => Self::Call,
            CallKind::Construct => Self::Construct,
            CallKind::Macro => Self::Macro,
            CallKind::Super => Self::Super,
        }
    }
}

impl CallKindCompat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Construct => "construct",
            Self::Macro => "macro",
            Self::Super => "super",
            Self::Implement => "implement",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphStats {
    pub function_count: usize,
    pub edge_count: usize,
    pub resolved_edge_count: usize,
    pub external_edge_count: usize,
    pub ambiguous_edge_count: usize,
    pub type_count: usize,
    pub build_ms: u64,
}

/// Per-callable file/line metadata. Cheaper than re-parsing on every
/// query and lets us hand consumers an accurate jump-to-source location
/// even for methods with no outgoing edges (trait sigs, abstract decls).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallableMeta {
    pub file: PathBuf,
    pub line: u32,
    /// `function` / `method` / `constructor` / `destructor` / `operator`
    /// — lifted from `Declaration::kind.as_str()` at build time.
    pub kind: String,
}

/// Per-type metadata captured during the build walk. Lets `callers <Type>`
/// surface implementations and constructions even though types aren't
/// callable nodes in the call graph proper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeMeta {
    /// Source-declaration kind: "class", "struct", "interface", "enum",
    /// "record", or "trait" (Rust). Lifted from `Declaration::native_kind`
    /// when present, else from `DeclarationKind`.
    pub kind: String,
    /// Repo-relative file path (for display + filtering).
    pub file: PathBuf,
    /// 1-indexed line of the type declaration.
    pub line: u32,
    /// Bases as written at the source — `Vec<String>` of the parent type
    /// names. Used to invert into "what implements this type".
    pub bases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallGraph {
    /// Schema-version stamp; rides on top of `JSON_SCHEMA_GRAPH_INDEX` so a
    /// breaking change to `CallEdge` shape forces a unified-cache rebuild.
    pub schema: String,
    /// All edges keyed by source qn. Empty `Vec` means "function exists but
    /// has no calls" — preserved so `callees` of a leaf returns gracefully.
    pub forward: HashMap<Qn, Vec<CallEdge>>,
    /// Inverted adjacency: target qn → list of edges pointing to it.
    /// Built once at the end of `build` so `callers` lookups are O(1).
    pub reverse: HashMap<Qn, Vec<CallEdge>>,
    /// Bare-name → list of qns map. The global symbol table from pass B,
    /// kept around for re-resolution after future per-file invalidation
    /// (not used in v1's full-rebuild model).
    pub symbol_table: HashMap<String, Vec<Qn>>,
    /// Per-callable file/line metadata, keyed by qn. Lets `callees <Type>`
    /// (and any other consumer) print the right source location for
    /// methods that have no outgoing edges (e.g. Rust trait method
    /// signatures with no default body).
    #[serde(default)]
    pub callable_meta: HashMap<Qn, CallableMeta>,
    /// Per-type metadata, keyed by qn. Powers the type-aware path of
    /// `callers <Type>`: implementations come from the bases-reverse index,
    /// constructions come from `forward` edges with `kind == Construct`.
    #[serde(default)]
    pub types: HashMap<Qn, TypeMeta>,
    /// Bare type-name → list of type qns. Lets `resolve_target_qns` find
    /// `LanguageAdapter` even though traits aren't in `symbol_table`.
    #[serde(default)]
    pub type_by_name: HashMap<String, Vec<Qn>>,
    /// Bare base-name → list of type qns that have it in their `bases`.
    /// O(1) "what implements this type" lookup.
    #[serde(default)]
    pub implementors: HashMap<String, Vec<Qn>>,
    pub root: PathBuf,
    pub built_at: SystemTime,
    pub stats: GraphStats,
}

impl CallGraph {
    pub fn empty(root: PathBuf) -> Self {
        Self {
            schema: JSON_SCHEMA_GRAPH_INDEX.to_string(),
            forward: HashMap::new(),
            reverse: HashMap::new(),
            symbol_table: HashMap::new(),
            callable_meta: HashMap::new(),
            types: HashMap::new(),
            type_by_name: HashMap::new(),
            implementors: HashMap::new(),
            root,
            built_at: SystemTime::now(),
            stats: GraphStats::default(),
        }
    }

    /// Single-pass invert of `forward` into `reverse`. Called once after the
    /// resolution passes complete.
    pub fn rebuild_reverse(&mut self) {
        let mut rev: HashMap<Qn, Vec<CallEdge>> = HashMap::new();
        for edges in self.forward.values() {
            for e in edges {
                if let CallTarget::Resolved(qn) = &e.target {
                    rev.entry(qn.clone()).or_default().push(e.clone());
                }
            }
        }
        for v in rev.values_mut() {
            v.sort_by(|a, b| a.source.0.cmp(&b.source.0).then(a.line.cmp(&b.line)));
        }
        self.reverse = rev;
    }


}
