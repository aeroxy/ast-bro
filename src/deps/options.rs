#![allow(dead_code)]

use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Plain text (depth-prefixed list or boxed tree).
    Text,
    /// Single-line text — for `graph`, just file→file lines.
    #[allow(dead_code)]
    Compact,
    /// JSON document with versioned schema header.
    Json { compact: bool },
    /// GraphViz DOT (only meaningful for `graph`).
    Dot,
    /// Design Structure Matrix — file × file binary matrix, sorted by Lakos level.
    Dsm,
}

#[derive(Debug, Clone)]
pub struct DepOptions {
    /// Repo root from which to walk and build the graph.
    pub root: PathBuf,
    /// Force a fresh build, ignoring any cached graph.
    pub rebuild: bool,
    /// Include unresolved imports (the `external` bucket) in output.
    pub include_external: bool,
    /// Max BFS depth for forward / reverse traversal commands.
    pub max_depth: usize,
    /// Cap for reverse-deps result count (popular files have huge fan-in).
    pub limit: usize,
    /// Cycle filter: SCCs smaller than this are dropped.
    pub min_cycle_size: usize,
    pub output: OutputMode,
}

impl Default for DepOptions {
    fn default() -> Self {
        Self {
            root: PathBuf::from(crate::defaults::ROOT),
            rebuild: false,
            include_external: false,
            max_depth: crate::defaults::FILE_DEPTH,
            limit: crate::defaults::LIMIT,
            min_cycle_size: crate::defaults::MIN_SIZE,
            output: OutputMode::Text,
        }
    }
}

#[derive(Debug)]
pub enum DepError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    NoFile {
        path: PathBuf,
    },
    NotInRoot {
        file: PathBuf,
        root: PathBuf,
    },
    BadFormat(String),
}

impl fmt::Display for DepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "i/o error reading {}: {}", path.display(), source)
            }
            Self::NoFile { path } => write!(f, "file not found: {}", path.display()),
            Self::NotInRoot { file, root } => write!(
                f,
                "{} is outside the project root {}",
                file.display(),
                root.display()
            ),
            Self::BadFormat(s) => write!(f, "{}", s),
        }
    }
}

impl std::error::Error for DepError {}
