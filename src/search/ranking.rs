//! Boosts and penalties applied after RRF fusion.
//!
//! All public functions operate on `HashMap<u32, f32>` keyed by chunk id (an
//! index into a `&[Chunk]` parallel array). Where the Python keeps `Chunk` as
//! the dict key (frozen dataclass identity), we use the integer id and pass
//! the chunks slice to functions that need `file_path` or `content`.

use crate::search::chunker::Chunk;
use crate::search::tokens::split_identifier;
use regex::{Regex, RegexBuilder};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

const EMBEDDED_STEM_MIN_LEN: usize = 4;
const EMBEDDED_SYMBOL_BOOST_SCALE: f32 = 0.5;
const DEFINITION_BOOST_MULTIPLIER: f32 = 3.0;
const STEM_BOOST_MULTIPLIER: f32 = 1.0;
const FILE_COHERENCE_BOOST_FRAC: f32 = 0.2;

const STRONG_PENALTY: f32 = 0.3;
const MODERATE_PENALTY: f32 = 0.5;
const MILD_PENALTY: f32 = 0.7;
const FILE_SATURATION_THRESHOLD: usize = 1;
const FILE_SATURATION_DECAY: f32 = 0.5;

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "do", "does", "for", "from",
    "has", "have", "how", "if", "in", "is", "it", "not", "of", "on", "or", "the",
    "to", "was", "what", "when", "where", "which", "who", "why", "with",
];

const DEFINITION_KEYWORDS: &[&str] = &[
    "class", "module", "defmodule", "def", "interface", "struct", "enum",
    "trait", "type", "func", "function", "object", "abstract class",
    "data class", "fn", "fun", "package", "namespace", "protocol",
    "record", "typedef",
];

const SQL_DEFINITION_KEYWORDS: &[&str] = &[
    "CREATE TABLE", "CREATE VIEW", "CREATE PROCEDURE", "CREATE FUNCTION",
];

const REEXPORT_FILENAMES: &[&str] = &["__init__.py", "package-info.java"];

// ────────────────────────────────────────────────────────────────────────────
// Static regexes
// ────────────────────────────────────────────────────────────────────────────

fn embedded_symbol_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let pattern = concat!(
            r"\b(?:",
            r"[A-Z][a-z][a-zA-Z0-9]*[A-Z][a-zA-Z0-9]*", // PascalCase
            "|",
            r"[a-z][a-zA-Z0-9]*[A-Z][a-zA-Z0-9]+", // camelCase
            r")\b",
        );
        Regex::new(pattern).expect("embedded_symbol_re")
    })
}

fn ident_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-zA-Z_][a-zA-Z0-9_]*").unwrap())
}

fn test_file_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let pattern = concat!(
            r"(?:^|/)(?:",
            // Python
            r"test_[^/]*\.py", "|", r"[^/]*_test\.py",
            // Go
            "|", r"[^/]*_test\.go",
            // Java
            "|", r"[^/]*Tests?\.java",
            // PHP
            "|", r"[^/]*Test\.php",
            // Ruby
            "|", r"[^/]*_spec\.rb", "|", r"[^/]*_test\.rb",
            // JS/TS
            "|", r"[^/]*\.test\.[jt]sx?", "|", r"[^/]*\.spec\.[jt]sx?",
            // Kotlin
            "|", r"[^/]*Tests?\.kt", "|", r"[^/]*Spec\.kt",
            // Swift
            "|", r"[^/]*Tests?\.swift", "|", r"[^/]*Spec\.swift",
            // C#
            "|", r"[^/]*Tests?\.cs",
            // C / C++
            "|", r"test_[^/]*\.cpp", "|", r"[^/]*_test\.cpp",
            "|", r"test_[^/]*\.c", "|", r"[^/]*_test\.c",
            // Scala
            "|", r"[^/]*Spec\.scala", "|", r"[^/]*Suite\.scala", "|", r"[^/]*Test\.scala",
            // Dart
            "|", r"[^/]*_test\.dart", "|", r"test_[^/]*\.dart",
            // Lua
            "|", r"[^/]*_spec\.lua", "|", r"[^/]*_test\.lua", "|", r"test_[^/]*\.lua",
            // Helpers
            "|", r"test_helpers?[^/]*\.\w+",
            ")$",
        );
        Regex::new(pattern).expect("test_file_re")
    })
}

fn test_dir_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|/)(?:tests?|__tests__|spec|testing)(?:/|$)").unwrap())
}

fn compat_dir_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|/)(?:compat|_compat|legacy)(?:/|$)").unwrap())
}

fn examples_dir_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|/)(?:_?examples?|docs?_src)(?:/|$)").unwrap())
}

fn type_defs_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.d\.ts$").unwrap())
}

/// Machine-generated files (protobuf/gRPC stubs, Dart/codegen output,
/// minified bundles, gomock files, files under a `generated/` dir). These
/// are almost never the symbol an agent is looking for, yet a query like
/// `Send` against a protobuf-heavy repo otherwise surfaces a dozen `.pb.go`
/// stubs ahead of the hand-written implementation. Matched on the path so a
/// strong penalty pushes them below real source. Suffixes are anchored to
/// `$` and the directory marker requires an exact path component, so normal
/// names like `general.rs` / `genesis.rs` never match.
fn generated_file_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(concat!(
            "(?:",
            // protobuf / gRPC across languages
            r"\.pb\.go$|_grpc\.pb\.go$|\.pb\.cc$|\.pb\.h$|\.pb\.dart$|\.pb\.ts$",
            r"|_pb2\.pyi?$|_pb2_grpc\.py$|_pb\.rb$|\.pbobjc\.[hm]$",
            // Dart / Flutter codegen
            r"|\.g\.dart$|\.freezed\.dart$|\.gr\.dart$",
            // C# designer / generated
            r"|\.Designer\.cs$|\.g\.cs$|\.g\.i\.cs$",
            // generic generated markers
            r"|\.generated\.[A-Za-z0-9]+$|_generated\.[A-Za-z0-9]+$|\.gen\.(?:go|ts|tsx|js|rs)$",
            // gomock / mockgen output
            r"|_mocks?\.go$|(?:^|/)mock_[^/]+\.go$",
            // minified / bundled JS/CSS
            r"|\.min\.js$|\.min\.css$|\.bundle\.js$",
            // directory markers (exact component)
            r"|(?:^|/)(?:generated|__generated__|\.gen)/",
            ")"
        ))
        .expect("generated_file_re")
    })
}

// ────────────────────────────────────────────────────────────────────────────
// Definition pattern cache
// ────────────────────────────────────────────────────────────────────────────

/// Two regexes per symbol — general (case-sensitive) and SQL (case-insensitive).
type DefnPair = (Regex, Regex);

fn definition_pattern(symbol_name: &str) -> DefnPair {
    static CACHE: OnceLock<Mutex<HashMap<String, DefnPair>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap();
    if let Some(p) = guard.get(symbol_name) {
        return p.clone();
    }
    let escaped = regex::escape(symbol_name);
    // The Python uses `(?<=\s)` lookbehind which the `regex` crate doesn't
    // support. `(?:^|\s)` is equivalent semantically — both match "keyword is
    // at start of string OR preceded by whitespace" — but consumes the
    // whitespace into the match, which is fine because we only need a match,
    // not the offset.
    let prefix = r"(?:^|\s)(?:";
    let suffix = format!(
        r")\s+(?:[A-Za-z_][A-Za-z0-9_]*(?:\.|::))*{escaped}(?:\s|[<({{:\[;]|$)",
    );
    let general_body: String = DEFINITION_KEYWORDS
        .iter()
        .map(|k| regex::escape(k))
        .collect::<Vec<_>>()
        .join("|");
    let sql_body: String = SQL_DEFINITION_KEYWORDS
        .iter()
        .map(|k| regex::escape(k))
        .collect::<Vec<_>>()
        .join("|");
    let general = RegexBuilder::new(&format!("{prefix}{general_body}{suffix}"))
        .multi_line(true)
        .build()
        .expect("general definition regex");
    let sql = RegexBuilder::new(&format!("{prefix}{sql_body}{suffix}"))
        .multi_line(true)
        .case_insensitive(true)
        .build()
        .expect("sql definition regex");
    let pair = (general, sql);
    guard.insert(symbol_name.to_string(), pair.clone());
    pair
}

fn chunk_defines_symbol(content: &str, symbol_name: &str) -> bool {
    let (general, sql) = definition_pattern(symbol_name);
    general.is_match(content) || sql.is_match(content)
}

fn stem_matches(stem: &str, name: &str) -> bool {
    let stem_norm: String = stem.replace('_', "");
    stem == name
        || stem_norm == name
        || stem.trim_end_matches('s') == name
        || stem_norm.trim_end_matches('s') == name
}

fn extract_symbol_name(query: &str) -> &str {
    let trimmed = query.trim();
    for sep in ["::", "\\", "->", "."].iter() {
        if let Some(idx) = trimmed.rfind(sep) {
            return &trimmed[idx + sep.len()..];
        }
    }
    trimmed
}

// ────────────────────────────────────────────────────────────────────────────
// File coherence boost
// ────────────────────────────────────────────────────────────────────────────

/// Promote files with multiple high-scoring chunks by boosting their top chunk.
/// Direct port of `boost_multi_chunk_files`.
pub fn boost_multi_chunk_files(scores: &mut HashMap<u32, f32>, chunks: &[Chunk]) {
    if scores.is_empty() {
        return;
    }
    let max_score = scores.values().cloned().fold(0.0f32, f32::max);
    if max_score == 0.0 {
        return;
    }

    let mut file_sum: HashMap<&str, f32> = HashMap::new();
    let mut best_chunk: HashMap<&str, u32> = HashMap::new();
    for (&id, &score) in scores.iter() {
        let path: &str = chunks[id as usize].file_path.as_str();
        *file_sum.entry(path).or_insert(0.0) += score;
        match best_chunk.get(path) {
            Some(&prev) if score > scores[&prev] => {
                best_chunk.insert(path, id);
            }
            None => {
                best_chunk.insert(path, id);
            }
            _ => {}
        }
    }

    let max_file_sum = file_sum.values().cloned().fold(0.0f32, f32::max);
    if max_file_sum == 0.0 {
        return;
    }
    let boost_unit = max_score * FILE_COHERENCE_BOOST_FRAC;
    for (path, &id) in best_chunk.iter() {
        let extra = boost_unit * file_sum[path] / max_file_sum;
        if let Some(s) = scores.get_mut(&id) {
            *s += extra;
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Query-aware boosts (definition / stem / embedded symbol)
// ────────────────────────────────────────────────────────────────────────────

/// Apply query-type boosts to candidate scores. Returns a new map (Python
/// version mutates a copy of the input dict).
pub fn apply_query_boost(
    combined_scores: HashMap<u32, f32>,
    query: &str,
    all_chunks: &[Chunk],
) -> HashMap<u32, f32> {
    if combined_scores.is_empty() {
        return combined_scores;
    }
    let max_score = combined_scores.values().cloned().fold(0.0f32, f32::max);
    let mut boosted = combined_scores;

    if crate::search::fusion::is_symbol_query(query) {
        boost_symbol_definitions(&mut boosted, query, max_score, all_chunks);
    } else {
        boost_stem_matches(&mut boosted, query, max_score, all_chunks);
        boost_embedded_symbols(&mut boosted, query, max_score, all_chunks);
    }
    boosted
}

fn definition_tier(content: &str, file_path: &str, names: &[&str], boost_unit: f32) -> f32 {
    if !names.iter().any(|n| chunk_defines_symbol(content, n)) {
        return 0.0;
    }
    let stem = Path::new(file_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let stem_match = names.iter().any(|n| stem_matches(&stem, &n.to_ascii_lowercase()));
    boost_unit * if stem_match { 1.5 } else { 1.0 }
}

fn boost_symbol_definitions(
    boosted: &mut HashMap<u32, f32>,
    query: &str,
    max_score: f32,
    all_chunks: &[Chunk],
) {
    let trimmed = query.trim();
    let symbol_name = extract_symbol_name(query);
    let mut names: Vec<&str> = vec![symbol_name];
    if symbol_name != trimmed {
        names.push(trimmed);
    }
    let boost_unit = max_score * DEFINITION_BOOST_MULTIPLIER;

    // Boost candidates that already made the cut.
    let candidate_ids: Vec<u32> = boosted.keys().copied().collect();
    for id in &candidate_ids {
        let chunk = &all_chunks[*id as usize];
        let tier = definition_tier(&chunk.content, &chunk.file_path, &names, boost_unit);
        if tier > 0.0 {
            *boosted.get_mut(id).unwrap() += tier;
        }
    }

    // Scan non-candidates whose stem matches the symbol
    let symbol_lower = symbol_name.to_ascii_lowercase();
    for (id, chunk) in all_chunks.iter().enumerate() {
        let id = id as u32;
        if boosted.contains_key(&id) {
            continue;
        }
        let stem = Path::new(&chunk.file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !stem_matches(&stem, &symbol_lower) {
            continue;
        }
        let tier = definition_tier(&chunk.content, &chunk.file_path, &names, boost_unit);
        if tier > 0.0 {
            boosted.insert(id, tier);
        }
    }
}

fn boost_embedded_symbols(
    boosted: &mut HashMap<u32, f32>,
    query: &str,
    max_score: f32,
    all_chunks: &[Chunk],
) {
    let names: Vec<String> = embedded_symbol_re()
        .find_iter(query)
        .map(|m| m.as_str().to_string())
        .collect();
    if names.is_empty() {
        return;
    }
    let names_ref: Vec<&str> = names.iter().map(String::as_str).collect();
    let boost_unit =
        max_score * DEFINITION_BOOST_MULTIPLIER * EMBEDDED_SYMBOL_BOOST_SCALE;

    let candidate_ids: Vec<u32> = boosted.keys().copied().collect();
    for id in &candidate_ids {
        let chunk = &all_chunks[*id as usize];
        let tier = definition_tier(&chunk.content, &chunk.file_path, &names_ref, boost_unit);
        if tier > 0.0 {
            *boosted.get_mut(id).unwrap() += tier;
        }
    }

    let symbols_lower: Vec<String> = names.iter().map(|s| s.to_ascii_lowercase()).collect();
    for (id, chunk) in all_chunks.iter().enumerate() {
        let id = id as u32;
        if boosted.contains_key(&id) {
            continue;
        }
        let stem = Path::new(&chunk.file_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let stem_norm: String = stem.replace('_', "");
        let stem_ok = symbols_lower.iter().any(|sym| {
            stem == *sym
                || stem_norm == *sym
                || (stem.len() >= EMBEDDED_STEM_MIN_LEN && sym.starts_with(&stem))
                || (stem_norm.len() >= EMBEDDED_STEM_MIN_LEN && sym.starts_with(&stem_norm))
        });
        if !stem_ok {
            continue;
        }
        let tier = definition_tier(&chunk.content, &chunk.file_path, &names_ref, boost_unit);
        if tier > 0.0 {
            boosted.insert(id, tier);
        }
    }
}

fn count_keyword_matches(keywords: &HashSet<String>, parts: &HashSet<String>) -> usize {
    let exact: HashSet<&String> = keywords.intersection(parts).collect();
    if exact.len() == keywords.len() {
        return exact.len();
    }
    let mut n = exact.len();
    for k in keywords.iter().filter(|k| !exact.contains(k)) {
        for p in parts {
            let (shorter, longer) = if k.len() <= p.len() { (k, p) } else { (p, k) };
            if shorter.len() >= 3 && longer.starts_with(shorter.as_str()) {
                n += 1;
                break;
            }
        }
    }
    n
}

fn boost_stem_matches(
    boosted: &mut HashMap<u32, f32>,
    query: &str,
    max_score: f32,
    all_chunks: &[Chunk],
) {
    let stop: HashSet<&str> = STOPWORDS.iter().copied().collect();
    let keywords: HashSet<String> = ident_re()
        .find_iter(query)
        .map(|m| m.as_str().to_ascii_lowercase())
        .filter(|w| w.len() > 2 && !stop.contains(w.as_str()))
        .collect();
    if keywords.is_empty() {
        return;
    }
    let boost = max_score * STEM_BOOST_MULTIPLIER;
    let mut path_cache: HashMap<&str, HashSet<String>> = HashMap::new();

    let candidate_ids: Vec<u32> = boosted.keys().copied().collect();
    for id in candidate_ids {
        let chunk = &all_chunks[id as usize];
        let path: &str = chunk.file_path.as_str();
        let parts = path_cache.entry(path).or_insert_with(|| {
            let p = Path::new(path);
            let mut parts: HashSet<String> = HashSet::new();
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            parts.insert(stem.to_ascii_lowercase());
            parts.extend(split_identifier(stem));
            if let Some(parent) = p.parent().and_then(|d| d.file_name()).and_then(|s| s.to_str()) {
                if !matches!(parent, "" | "." | "/" | "..") {
                    parts.insert(parent.to_ascii_lowercase());
                    parts.extend(split_identifier(parent));
                }
            }
            parts
        });
        let n = count_keyword_matches(&keywords, parts);
        if n > 0 {
            let ratio = n as f32 / keywords.len() as f32;
            if ratio >= 0.10 {
                if let Some(s) = boosted.get_mut(&id) {
                    *s += boost * ratio;
                }
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Penalties + final top-k selection
// ────────────────────────────────────────────────────────────────────────────

fn file_path_penalty(file_path: &str) -> f32 {
    let normalised: String = file_path.replace('\\', "/");
    let mut penalty = 1.0f32;
    if test_file_re().is_match(&normalised) || test_dir_re().is_match(&normalised) {
        penalty *= STRONG_PENALTY;
    }
    let basename = Path::new(file_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if REEXPORT_FILENAMES.contains(&basename) {
        penalty *= MODERATE_PENALTY;
    }
    if compat_dir_re().is_match(&normalised) {
        penalty *= STRONG_PENALTY;
    }
    if examples_dir_re().is_match(&normalised) {
        penalty *= STRONG_PENALTY;
    }
    if type_defs_re().is_match(&normalised) {
        penalty *= MILD_PENALTY;
    }
    if generated_file_re().is_match(&normalised) {
        penalty *= STRONG_PENALTY;
    }
    penalty
}

/// Greedy top-k selection with file-path penalties and file-saturation decay.
/// Returns `(chunk_id, final_score)` pairs sorted by score descending.
pub fn rerank_topk(
    scores: &HashMap<u32, f32>,
    chunks: &[Chunk],
    top_k: usize,
    penalise_paths: bool,
) -> Vec<(u32, f32)> {
    if scores.is_empty() || top_k == 0 {
        return Vec::new();
    }

    // Apply path penalties (cached per file).
    let mut penalty_cache: HashMap<&str, f32> = HashMap::new();
    let mut penalised: Vec<(u32, f32)> = Vec::with_capacity(scores.len());
    for (&id, &score) in scores {
        let path: &str = chunks[id as usize].file_path.as_str();
        let pen = if penalise_paths {
            *penalty_cache
                .entry(path)
                .or_insert_with(|| file_path_penalty(path))
        } else {
            1.0
        };
        penalised.push((id, score * pen));
    }

    // Sort by penalised score descending (stable on ties — preserves insertion
    // order, matching Python's stable `sorted`).
    penalised.sort_by(|a, b| {
        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
    });

    // Greedy selection with file-saturation decay. We only need to walk far
    // enough to find top_k results that survive the decay.
    let mut file_selected: HashMap<&str, usize> = HashMap::new();
    let mut selected: Vec<(f32, u32)> = Vec::with_capacity(top_k);
    let mut min_selected = f32::INFINITY;

    for (id, pen_score) in penalised {
        if selected.len() >= top_k && pen_score <= min_selected {
            break;
        }
        let path: &str = chunks[id as usize].file_path.as_str();
        let already = *file_selected.get(path).unwrap_or(&0);
        let mut eff = pen_score;
        if already >= FILE_SATURATION_THRESHOLD {
            let excess = (already - FILE_SATURATION_THRESHOLD + 1) as i32;
            eff *= FILE_SATURATION_DECAY.powi(excess);
        }
        selected.push((eff, id));
        file_selected.insert(path, already + 1);

        if selected.len() >= top_k {
            min_selected = selected.iter().map(|t| t.0).fold(f32::INFINITY, f32::min);
        }
    }

    selected.sort_by(|a, b| {
        b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
    });
    selected.into_iter().take(top_k).map(|(s, id)| (id, s)).collect()
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn ck(_id: u32, file_path: &str, content: &str) -> Chunk {
        Chunk {
            content: content.to_string(),
            file_path: file_path.to_string(),
            start_line: 1,
            end_line: 1,
            start_byte: 0,
            end_byte: content.len() as u32,
            language: "rust".to_string(),
        }
    }

    // ── stem_matches / extract_symbol_name ────────────────────────────────

    #[test]
    fn extract_symbol_unqualified() {
        assert_eq!(extract_symbol_name("Client"), "Client");
        assert_eq!(extract_symbol_name("  Foo  "), "Foo");
    }

    #[test]
    fn extract_symbol_namespaced() {
        assert_eq!(extract_symbol_name("Sinatra::Base"), "Base");
        assert_eq!(extract_symbol_name(r"My\Namespace\Class"), "Class");
        assert_eq!(extract_symbol_name("Foo->bar"), "bar");
        assert_eq!(extract_symbol_name("a.b.c"), "c");
    }

    #[test]
    fn stem_matches_variants() {
        assert!(stem_matches("foo", "foo"));
        assert!(stem_matches("my_func", "myfunc"));   // snake collapsed
        assert!(stem_matches("foos", "foo"));          // plural
        assert!(stem_matches("my_funcs", "myfunc"));   // plural + snake
        assert!(!stem_matches("bar", "foo"));
    }

    // ── chunk_defines_symbol ──────────────────────────────────────────────

    #[test]
    fn defines_basic_class() {
        assert!(chunk_defines_symbol("class Client:\n    pass", "Client"));
        assert!(chunk_defines_symbol("def parse(): pass", "parse"));
        assert!(chunk_defines_symbol("struct Foo {}", "Foo"));
    }

    #[test]
    fn defines_namespaced() {
        assert!(chunk_defines_symbol("defmodule Phoenix.Router do\nend", "Router"));
    }

    #[test]
    fn defines_sql_case_insensitive() {
        assert!(chunk_defines_symbol("CREATE TABLE users (id INT);", "users"));
        assert!(chunk_defines_symbol("create table Users (id int);", "Users"));
    }

    #[test]
    fn does_not_define_when_referenced() {
        assert!(!chunk_defines_symbol("client = Client()", "Client"));
    }

    // ── file_path_penalty ─────────────────────────────────────────────────

    #[test]
    fn penalty_test_files() {
        assert!((file_path_penalty("tests/test_foo.py") - STRONG_PENALTY).abs() < 1e-6);
        assert!((file_path_penalty("foo_test.go") - STRONG_PENALTY).abs() < 1e-6);
        assert!((file_path_penalty("FooTest.java") - STRONG_PENALTY).abs() < 1e-6);
        assert!((file_path_penalty("src/foo.test.ts") - STRONG_PENALTY).abs() < 1e-6);
        // tests/ dir alone (file doesn't match the test_file pattern) — still penalized.
        assert!((file_path_penalty("tests/util.py") - STRONG_PENALTY).abs() < 1e-6);
    }

    #[test]
    fn penalty_compat_dir() {
        assert!((file_path_penalty("legacy/foo.py") - STRONG_PENALTY).abs() < 1e-6);
        assert!((file_path_penalty("compat/foo.rs") - STRONG_PENALTY).abs() < 1e-6);
    }

    #[test]
    fn penalty_d_ts() {
        assert!((file_path_penalty("types/foo.d.ts") - MILD_PENALTY).abs() < 1e-6);
    }

    #[test]
    fn penalty_init_py() {
        assert!((file_path_penalty("pkg/__init__.py") - MODERATE_PENALTY).abs() < 1e-6);
    }

    #[test]
    fn penalty_normal_file() {
        assert_eq!(file_path_penalty("src/main.rs"), 1.0);
    }

    #[test]
    fn penalty_generated_files() {
        for p in [
            "api/user.pb.go",
            "proto/user_grpc.pb.go",
            "gen/user_pb2.py",
            "lib/models/user.g.dart",
            "service/user_mocks.go",
            "internal/mock_store.go",
            "static/app.min.js",
            "generated/client.ts",
        ] {
            assert!(
                (file_path_penalty(p) - STRONG_PENALTY).abs() < 1e-6,
                "{p} should get the generated-file penalty"
            );
        }
        // Look-alikes that must NOT be penalised.
        for p in ["src/general.rs", "src/genesis.rs", "src/codegen.rs"] {
            assert_eq!(file_path_penalty(p), 1.0, "{p} must not be penalised");
        }
    }

    // ── boost_multi_chunk_files ───────────────────────────────────────────

    #[test]
    fn coherence_boosts_top_chunk_per_file() {
        let chunks = vec![
            ck(0, "src/a.rs", ""),
            ck(1, "src/a.rs", ""),
            ck(2, "src/b.rs", ""),
        ];
        let mut scores: HashMap<u32, f32> = [(0u32, 1.0f32), (1u32, 0.5f32), (2u32, 0.4f32)]
            .into_iter()
            .collect();
        let pre_top = scores[&0];
        boost_multi_chunk_files(&mut scores, &chunks);
        // a.rs has higher file_sum (1.5) than b.rs (0.4); top chunk of a.rs (id 0) gets the bigger boost.
        assert!(scores[&0] > pre_top);
        // id 1 untouched (not the top chunk in its file).
        assert!((scores[&1] - 0.5).abs() < 1e-6);
    }

    // ── rerank_topk ────────────────────────────────────────────────────────

    #[test]
    fn rerank_applies_path_penalties() {
        let chunks = vec![
            ck(0, "src/main.rs", ""),
            ck(1, "tests/test_foo.py", ""),
        ];
        let scores: HashMap<u32, f32> = [(0u32, 1.0f32), (1u32, 1.5f32)].into_iter().collect();
        // Without penalties, id 1 wins.
        let no_pen = rerank_topk(&scores, &chunks, 2, /* penalise_paths */ false);
        assert_eq!(no_pen[0].0, 1);
        // With penalties, the test file (1.5 * 0.3 * 0.3 = 0.135) drops below main.rs (1.0).
        let with_pen = rerank_topk(&scores, &chunks, 2, /* penalise_paths */ true);
        assert_eq!(with_pen[0].0, 0);
    }

    #[test]
    fn rerank_saturation_decay() {
        // Three chunks, all from a.rs. With saturation threshold = 1, the 2nd
        // and 3rd get decayed.
        let chunks = vec![
            ck(0, "src/a.rs", ""),
            ck(1, "src/a.rs", ""),
            ck(2, "src/a.rs", ""),
        ];
        let scores: HashMap<u32, f32> = [(0u32, 1.0f32), (1u32, 0.9f32), (2u32, 0.8f32)]
            .into_iter()
            .collect();
        let out = rerank_topk(&scores, &chunks, 3, false);
        // id 0 first (1.0), id 1 second but * 0.5 = 0.45, id 2 third * 0.25 = 0.2.
        assert_eq!(out[0].0, 0);
        assert!((out[0].1 - 1.0).abs() < 1e-6);
        // Decayed values present.
        assert!(out[1].1 < 0.5);
        assert!(out[2].1 < 0.25);
    }

    #[test]
    fn rerank_top_k_limits_output() {
        let chunks = vec![ck(0, "a", ""), ck(1, "b", ""), ck(2, "c", "")];
        let scores: HashMap<u32, f32> = [(0u32, 0.3f32), (1u32, 0.2f32), (2u32, 0.1f32)]
            .into_iter()
            .collect();
        let out = rerank_topk(&scores, &chunks, 2, false);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, 0);
        assert_eq!(out[1].0, 1);
    }

    // ── apply_query_boost (end-to-end) ────────────────────────────────────

    #[test]
    fn boost_promotes_definition_for_symbol_query() {
        let chunks = vec![
            ck(0, "src/handler.rs", "fn use_handler() { stack.handle() }"),  // uses
            ck(1, "src/stack.rs", "struct HandlerStack { items: Vec<u32> }"),  // defines
        ];
        let scores: HashMap<u32, f32> = [(0u32, 0.5f32), (1u32, 0.3f32)].into_iter().collect();
        let boosted = apply_query_boost(scores, "HandlerStack", &chunks);
        // The defining chunk should now outrank the using chunk.
        assert!(boosted[&1] > boosted[&0]);
    }

    #[test]
    fn boost_stem_matches_for_nl_query() {
        let chunks = vec![
            ck(0, "src/auth/login.rs", "fn login() {}"),
            ck(1, "src/index.rs", "fn main() {}"),
        ];
        let scores: HashMap<u32, f32> = [(0u32, 0.5f32), (1u32, 0.5f32)].into_iter().collect();
        // NL query mentioning "login" should boost the login.rs chunk.
        let boosted = apply_query_boost(scores, "how does login work", &chunks);
        assert!(boosted[&0] > boosted[&1]);
    }
}
