//! One error contract for every subcommand (issues #33 / #36).
//!
//! - **Channel**: stdout carries results only. Every rejection, note, hint,
//!   and warning goes to stderr, so `--json` output always parses and
//!   "stdout is empty" is a reliable signal on its own.
//! - **Exit code**: `0` — the query ran and the result is complete,
//!   including a legitimately empty result. `2` — the query could not run
//!   as asked (no such path, no such symbol, unknown flag, no input after
//!   expansion); nothing on stdout. `1` — internal failure.
//! - **Machine-readable form**: when `--json` was requested, a rejection
//!   also emits an `ast-bro.error.v1` object on stderr, same envelope for
//!   every subcommand, so a consumer needs exactly one check.

use serde_json::json;

pub const JSON_SCHEMA_ERROR: &str = "ast-bro.error.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The argument list itself was empty (e.g. a shell substitution
    /// collapsed) — nothing was inspected.
    NoInput,
    PathNotFound,
    SymbolNotFound,
    UnknownFlag,
    BadArgument,
    /// Internal failure: parse crash, unreadable index/cache. Exits 1.
    IndexError,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoInput => "no_input",
            Self::PathNotFound => "path_not_found",
            Self::SymbolNotFound => "symbol_not_found",
            Self::UnknownFlag => "unknown_flag",
            Self::BadArgument => "bad_argument",
            Self::IndexError => "index_error",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Self::IndexError => 1,
            _ => 2,
        }
    }
}

pub struct CliError {
    pub command: String,
    pub kind: ErrorKind,
    /// First line of the message: what arrived and why it is an error
    /// rather than a result.
    pub detail: String,
    /// Optional recovery line: what to do instead.
    pub hint: Option<String>,
    /// Extra fields merged into the JSON envelope (e.g. `missing` paths).
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl CliError {
    pub fn new(command: impl Into<String>, kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            kind,
            detail: detail.into(),
            hint: None,
            extra: serde_json::Map::new(),
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn extra(mut self, key: &str, value: serde_json::Value) -> Self {
        self.extra.insert(key.to_string(), value);
        self
    }

    /// The `ast-bro.error.v1` envelope — the single place its shape lives,
    /// so the parse-error path and `emit` cannot drift apart.
    pub fn envelope(&self) -> serde_json::Value {
        let mut doc = json!({
            "schema": JSON_SCHEMA_ERROR,
            "command": self.command,
            "kind": self.kind.as_str(),
            "detail": self.detail,
        });
        if let Some(h) = &self.hint {
            doc["hint"] = json!(h);
        }
        for (k, v) in &self.extra {
            doc[k.as_str()] = v.clone();
        }
        doc
    }

    /// Envelope only, no human text — for call sites that already printed a
    /// rendered message (e.g. clap parse errors).
    pub fn emit_json_only(&self) {
        eprintln!("{}", self.envelope());
    }

    /// Print the rejection to stderr — human text always, plus the JSON
    /// envelope when the caller asked for `--json` — and return the exit
    /// code to use. Never touches stdout.
    ///
    /// The human text is unconditional, so a call site that has already
    /// rendered its own message (the clap parse path, which lets
    /// `clap::Error::print` do it) must use `emit_json_only` instead or the
    /// rejection prints twice.
    pub fn emit(&self, json_mode: bool) -> i32 {
        eprintln!("error: {}", self.detail);
        if let Some(h) = &self.hint {
            for line in h.lines() {
                eprintln!("  {}", line);
            }
        }
        if json_mode {
            self.emit_json_only();
        }
        self.kind.exit_code()
    }

    /// `emit` + `std::process::exit` for call sites with no cleanup needs.
    pub fn exit(&self, json_mode: bool) -> ! {
        std::process::exit(self.emit(json_mode));
    }
}

/// The shared "no such symbol" rejection used by every symbol-query
/// subcommand (`callers`, `callees`, `trace`, `impact`, `context`).
pub fn symbol_not_found(command: &str, target: &str, json_mode: bool) -> i32 {
    CliError::new(
        command,
        ErrorKind::SymbolNotFound,
        format!("no symbol matches '{}'", target),
    )
    .hint("Try a more specific suffix like 'Type.method', or `ast-bro search` to locate the name.")
    .emit(json_mode)
}
