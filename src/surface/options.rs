use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Flat,
    Tree,
    Json { compact: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LangOverride {
    Rust,
    Python,
    TypeScript,
    Scala,
    Fallback,
}

impl LangOverride {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "rust" | "rs" => Some(Self::Rust),
            "python" | "py" => Some(Self::Python),
            "typescript" | "ts" | "javascript" | "js" => Some(Self::TypeScript),
            "scala" => Some(Self::Scala),
            "fallback" | "generic" => Some(Self::Fallback),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SurfaceOptions {
    pub output: OutputMode,
    /// Visibility filter passed through to the fallback resolver.
    /// Ignored for Rust / Python (they always honour their own
    /// language semantics).
    pub include_private: bool,
    /// Recursion guard for `pub use` chains and Python re-export hops.
    pub max_depth: usize,
    /// When emitting flat text, also append the via-chain on each line.
    pub include_chain: bool,
    /// Force a specific resolver instead of auto-detecting from manifest.
    pub lang_override: Option<LangOverride>,
}

impl Default for SurfaceOptions {
    fn default() -> Self {
        Self {
            output: OutputMode::Flat,
            include_private: false,
            max_depth: crate::defaults::SURFACE_MAX_DEPTH,
            include_chain: false,
            lang_override: None,
        }
    }
}

#[derive(Debug)]
pub enum SurfaceError {
    NoEntryPoint { path: PathBuf, hint: String },
    Io { path: PathBuf, source: std::io::Error },
    Parse { path: PathBuf, message: String },
    #[allow(dead_code)]
    BadOverride(String),
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoEntryPoint { path, hint } => write!(
                f,
                "no recognizable package entry point under {}: {}",
                path.display(),
                hint
            ),
            Self::Io { path, source } => {
                write!(f, "i/o error reading {}: {}", path.display(), source)
            }
            Self::Parse { path, message } => {
                write!(f, "parse error in {}: {}", path.display(), message)
            }
            Self::BadOverride(s) => write!(f, "unknown --lang value: {}", s),
        }
    }
}

impl std::error::Error for SurfaceError {}
