//! Log/text token compressor — the pure engine behind `sb squeeze`.
//!
//! A dependency-light, I/O-free function that collapses the repetitive structure
//! of logs (timestamps, component tags, repeated token and tag runs) into short
//! tags, returning the legend as structured `(tag, value)` pairs so the output
//! round-trips back to the original.
//!
//! Pipeline (all stages kept — the input is logs, where most content is real):
//! ```text
//!   1. Leading-zero trim          0x0001 -> 0x1               (lossy, no legend)
//!   2. Timestamp dictionary       most-frequent ISO8601 -> #T#
//!   3. Component / key extraction frequent [Tag] / key= -> #0#
//!   4. Base62 tag names           #10# packs as #a# to keep tags narrow
//!   5a. BPE (normal)              repeated token runs       -> #n#
//!   5b. BPE (meta)                repeated runs *of tags*   -> !n!
//!   6. Macro templating           lines differing by 1 tag  -> &n + &n:val
//!   dedup                         identical lines collapse  -> ... xN (lossy)
//! ```
//! (An earlier design had a 7th "tag-sequence macro" stage, but its matcher
//! needs a `\1` backreference the `regex` crate rejects, so it never fired and
//! was dropped rather than carried as dead code.)
//!
//! `regex` is the only external crate (already a dependency of this repo);
//! `std::sync::LazyLock` holds the one-off compiled patterns.
//!
//! ## Determinism
//! Tag assignment is fully deterministic so output is stable and testable.
//! Anywhere a tie could otherwise fall to `HashMap` iteration order
//! (frequent-pattern ordering, BPE best-phrase selection, template ordering)
//! we add an explicit, content-derived tie-breaker. Equal-savings candidates
//! are resolved by their text (lexicographic), never by hash seed or clock.

pub mod render;

use regex::Regex;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

const BASE62: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
const RADIX: usize = 62;

// Tuning parameters for compression.
const BPE_MAX_ITERATIONS: usize = 100;
const BPE_MIN_SAVINGS: i32 = 5;
const MACRO_MIN_COUNT: usize = 4;
const MACRO_MIN_TEMPLATE_LEN: usize = 5;
const MACRO_OVERHEAD_MULT: i32 = 4;
const MACRO_OVERHEAD_CONST: i32 = 5;

// Pre-compiled static regexes for one-off passes.
static RE_TAGS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(#(?:T|[0-9a-zA-Z]+)#|![0-9a-zA-Z]+!)").unwrap());
static RE_ZEROS_1: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b0{3,}([1-9A-Fa-f][0-9A-Fa-f]*\b)").unwrap());
static RE_ZEROS_2: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b0{3,}(0\b)").unwrap());
static RE_ZEROS_3: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\b0x)0{3,}([1-9A-Fa-f][0-9A-Fa-f]*\b)").unwrap());
static RE_ZEROS_4: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\b0x)0{3,}(0\b)").unwrap());
static RE_TS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:").unwrap());
static RE_COMP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[[A-Za-z0-9_-]+\]").unwrap());
static RE_KEYS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[A-Za-z0-9_]+={1,2}").unwrap());
static RE_NO_TIME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#T#\d{2}\.\d{3}\s*").unwrap());

/// Result of compressing a chunk of log/text.
pub struct Squeezed {
    /// Compressed text (the deduplicated body — no legend prepended).
    pub body: String,
    /// `(tag, original)` pairs in display / assignment order. Reversing these
    /// from last to first re-expands the body. The legend is reversible; the
    /// only lossy stages (leading-zero trim, line dedup) leave no entry.
    pub legend: Vec<(String, String)>,
}

/// Compress `input`. Pure, no I/O. Empty / whitespace-only input round-trips
/// unchanged with an empty legend.
pub fn squeeze(input: &str) -> Squeezed {
    let mut c = Compressor::new();
    let body = c.compress(input.to_string());
    Squeezed {
        body,
        legend: c.legend.into_iter().map(split_legend_entry).collect(),
    }
}

/// Split a `"tag = value"` legend line into `(tag, value)` on the *first*
/// " = " — tags never contain " = ", so a value that does is preserved intact.
fn split_legend_entry(entry: String) -> (String, String) {
    match entry.find(" = ") {
        Some(idx) => (entry[..idx].to_string(), entry[idx + 3..].to_string()),
        None => (entry, String::new()),
    }
}

#[inline]
fn advance_while(bytes: &[u8], mut i: usize, cond: impl Fn(u8) -> bool) -> usize {
    while i < bytes.len() && cond(bytes[i]) {
        i += 1;
    }
    i
}

#[inline]
fn calc_savings(count: usize, len: usize) -> i32 {
    (count as i32 - 1) * (len as i32) - MACRO_OVERHEAD_MULT * (count as i32) - MACRO_OVERHEAD_CONST
}

#[inline]
fn to_base62(mut n: usize) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let mut res = Vec::new();
    while n > 0 {
        res.push(BASE62[n % RADIX]);
        n /= RADIX;
    }
    res.reverse();
    String::from_utf8(res).expect("BASE62 is valid ASCII")
}

#[inline]
fn base62_len(mut n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let mut len = 0;
    while n > 0 {
        len += 1;
        n /= RADIX;
    }
    len
}

struct Compressor {
    var_idx: usize,
    meta_idx: usize,
    macro_idx: usize,
    legend: Vec<String>,
}

trait BpeStrategy {
    fn tokenize<'a>(&self, text: &'a str) -> Vec<&'a str>;
    fn split_text_with_separators<'a>(&self, text: &'a str) -> (Vec<&'a str>, Vec<&'a str>);
    fn max_n(&self) -> usize;
    fn min_trim_len(&self) -> usize;
    fn requires_hash(&self) -> bool;
    fn tag_len(&self, comp: &Compressor) -> usize;
    fn next_token(&self, comp: &mut Compressor) -> String;
}

struct NormalBpe;
impl BpeStrategy for NormalBpe {
    fn tokenize<'a>(&self, text: &'a str) -> Vec<&'a str> {
        let (mut tokens, bytes, mut i) = (Vec::new(), text.as_bytes(), 0);
        while i < bytes.len() {
            let is_alnum = bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_';
            let j = advance_while(bytes, i + 1, |b| {
                (b.is_ascii_alphanumeric() || b == b'_') == is_alnum
            });
            tokens.push(&text[i..j]);
            i = j;
        }
        tokens
    }

    fn split_text_with_separators<'a>(&self, text: &'a str) -> (Vec<&'a str>, Vec<&'a str>) {
        let (mut parts, mut seps, bytes) = (Vec::new(), Vec::new(), text.as_bytes());
        let (mut i, mut last_end) = (0, 0);
        while i < bytes.len() {
            if bytes[i] == b'#' {
                let mut j = i + 1;
                if j < bytes.len() && bytes[j] == b'T' {
                    j += 1;
                } else {
                    j = advance_while(bytes, j, |b| b.is_ascii_alphanumeric());
                }

                if j < bytes.len() && bytes[j] == b'#' && j > i + 1 {
                    parts.push(&text[last_end..i]);
                    seps.push(&text[i..=j]);
                    last_end = j + 1;
                    i = j + 1;
                    continue;
                }
            }
            i += 1;
        }
        parts.push(&text[last_end..]);
        (parts, seps)
    }

    fn max_n(&self) -> usize {
        20
    }
    fn min_trim_len(&self) -> usize {
        4
    }
    fn requires_hash(&self) -> bool {
        false
    }
    fn tag_len(&self, comp: &Compressor) -> usize {
        base62_len(comp.var_idx) + 2
    }
    fn next_token(&self, comp: &mut Compressor) -> String {
        comp.get_var_token()
    }
}

struct MetaBpe;
impl BpeStrategy for MetaBpe {
    fn tokenize<'a>(&self, text: &'a str) -> Vec<&'a str> {
        let (mut tokens, bytes, mut i) = (Vec::new(), text.as_bytes(), 0);
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'#' {
                let j = advance_while(bytes, i + 1, |c| c.is_ascii_alphanumeric());
                if j < bytes.len() && bytes[j] == b'#' {
                    tokens.push(&text[i..=j]);
                    i = j + 1;
                    continue;
                }
            } else if b == b'!' {
                let j = advance_while(bytes, i + 1, |c| c.is_ascii_alphanumeric());
                if j < bytes.len() && bytes[j] == b'!' {
                    tokens.push(&text[i..=j]);
                    i = j + 1;
                    continue;
                }
            }

            let is_alnum = b.is_ascii_alphanumeric() || b == b'_';
            let j = advance_while(bytes, i + 1, |c| {
                if is_alnum {
                    c.is_ascii_alphanumeric() || c == b'_'
                } else {
                    !c.is_ascii_alphanumeric() && c != b'_' && c != b'#' && c != b'!'
                }
            });
            tokens.push(&text[i..j]);
            i = j;
        }
        tokens
    }

    fn split_text_with_separators<'a>(&self, text: &'a str) -> (Vec<&'a str>, Vec<&'a str>) {
        let (mut parts, mut seps, bytes) = (Vec::new(), Vec::new(), text.as_bytes());
        let mut last_end = 0;
        for i in 0..bytes.len() {
            if bytes[i] == b'\n' {
                parts.push(&text[last_end..i]);
                seps.push("\n");
                last_end = i + 1;
            }
        }
        parts.push(&text[last_end..]);
        (parts, seps)
    }

    fn max_n(&self) -> usize {
        15
    }
    fn min_trim_len(&self) -> usize {
        5
    }
    fn requires_hash(&self) -> bool {
        true
    }
    fn tag_len(&self, comp: &Compressor) -> usize {
        base62_len(comp.meta_idx) + 2
    }
    fn next_token(&self, comp: &mut Compressor) -> String {
        comp.get_meta_token()
    }
}

impl Compressor {
    fn new() -> Self {
        Self {
            var_idx: 0,
            meta_idx: 1,
            macro_idx: 1,
            legend: Vec::new(),
        }
    }

    fn get_var_token(&mut self) -> String {
        let t = format!("#{}#", to_base62(self.var_idx));
        self.var_idx += 1;
        t
    }

    fn get_meta_token(&mut self) -> String {
        let t = format!("!{}!", to_base62(self.meta_idx));
        self.meta_idx += 1;
        t
    }

    fn get_macro_token(&mut self) -> String {
        let t = format!("&{}", to_base62(self.macro_idx));
        self.macro_idx += 1;
        t
    }

    fn replace_frequent(&mut self, text: &mut String, re: &Regex) {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for mat in re.find_iter(text) {
            *counts.entry(mat.as_str().to_string()).or_insert(0) += 1;
        }

        let mut sorted: Vec<_> = counts.into_iter().filter(|&(_, c)| c > 1).collect();
        // Deterministic: most-frequent first, ties broken by the pattern text
        // (a plain HashMap-order pick would be non-deterministic).
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

        let mut replacements: HashMap<String, String> = HashMap::with_capacity(sorted.len());
        for (pat, _) in &sorted {
            let token = self.get_var_token();
            self.legend.push(format!("{} = {}", token, pat));
            replacements.insert(pat.clone(), token);
        }

        let mut rewritten = String::with_capacity(text.len());
        let mut last_end = 0;
        for mat in re.find_iter(text) {
            rewritten.push_str(&text[last_end..mat.start()]);
            let matched = mat.as_str();
            if let Some(token) = replacements.get(matched) {
                rewritten.push_str(token);
            } else {
                rewritten.push_str(matched);
            }
            last_end = mat.end();
        }
        rewritten.push_str(&text[last_end..]);
        *text = rewritten;
    }

    fn run_bpe<S: BpeStrategy>(&mut self, text: String, max_iter: usize, strategy: S) -> String {
        let mut id_to_str: Vec<String> = Vec::new();
        let mut str_to_id: HashMap<String, u32> = HashMap::new();

        let mut get_or_add = |s: &str| -> u32 {
            *str_to_id.entry(s.to_string()).or_insert_with(|| {
                id_to_str.push(s.to_string());
                (id_to_str.len() - 1) as u32
            })
        };

        let (part_strs, seps) = strategy.split_text_with_separators(&text);
        let mut parts: Vec<Vec<u32>> = part_strs
            .into_iter()
            .map(|p| {
                if p.is_empty() {
                    vec![]
                } else {
                    strategy.tokenize(p).into_iter().map(&mut get_or_add).collect()
                }
            })
            .collect();

        for _ in 0..max_iter {
            let mut counts: HashMap<&[u32], usize> = HashMap::new();
            let max_n = strategy.max_n();

            for part in &parts {
                let len = part.len();
                if len < 2 {
                    continue;
                }

                for n in 2..=std::cmp::min(max_n, len) {
                    for i in 0..=(len - n) {
                        let slice = &part[i..(i + n)];
                        let (mut has_hash, mut total_len, mut valid) = (false, 0, true);

                        for &id in slice {
                            let s = &id_to_str[id as usize];
                            if s.contains('\n') || s.contains('\r') {
                                valid = false;
                                break;
                            }
                            if strategy.requires_hash() && s.contains('#') {
                                has_hash = true;
                            }
                            total_len += s.len();
                        }

                        if !valid || (strategy.requires_hash() && !has_hash) {
                            continue;
                        }

                        let first_s = &id_to_str[slice[0] as usize];
                        let last_s = &id_to_str[slice[slice.len() - 1] as usize];
                        let trim_len = total_len
                            - (first_s.len() - first_s.trim_start().len())
                            - (last_s.len() - last_s.trim_end().len());

                        if trim_len >= strategy.min_trim_len() {
                            *counts.entry(slice).or_insert(0) += 1;
                        }
                    }
                }
            }

            // Pick the highest-savings candidate. Determinism: iterating `counts`
            // (a HashMap) and keeping the first strict maximum would make
            // equal-savings ties depend on hash order, so we instead resolve
            // ties by the candidate's rendered phrase (lexicographic), which is
            // stable across runs.
            let mut best_slice: &[u32] = &[];
            let mut best_savings = 0;
            for (slice, &count) in &counts {
                if count > 1 {
                    let phrase_len: usize =
                        slice.iter().map(|&id| id_to_str[id as usize].len()).sum();
                    let tag_len = strategy.tag_len(self);
                    let savings = (count as i32) * (phrase_len as i32 - tag_len as i32)
                        - (tag_len as i32 + 3 + phrase_len as i32);

                    if savings > best_savings {
                        best_savings = savings;
                        best_slice = *slice;
                    } else if savings == best_savings && savings >= BPE_MIN_SAVINGS {
                        let to_bytes = |&id: &u32| id_to_str[id as usize].as_bytes();
                        let cand_bytes = slice.iter().flat_map(to_bytes);
                        let best_bytes = best_slice.iter().flat_map(to_bytes);
                        if cand_bytes.cmp(best_bytes) == std::cmp::Ordering::Less {
                            best_slice = *slice;
                        }
                    }
                }
            }

            if best_savings < BPE_MIN_SAVINGS {
                break;
            }

            let best_vec = best_slice.to_vec();
            let best_str: String = best_vec
                .iter()
                .map(|&id| id_to_str[id as usize].as_str())
                .collect();
            let token = strategy.next_token(self);

            let new_id = id_to_str.len() as u32;
            id_to_str.push(token.clone());
            str_to_id.insert(token.clone(), new_id);

            for part in &mut parts {
                if part.len() < best_vec.len() {
                    continue;
                }
                let (mut new_part, mut i) = (Vec::with_capacity(part.len()), 0);
                while i <= part.len() - best_vec.len() {
                    if &part[i..i + best_vec.len()] == best_vec.as_slice() {
                        new_part.push(new_id);
                        i += best_vec.len();
                    } else {
                        new_part.push(part[i]);
                        i += 1;
                    }
                }
                new_part.extend_from_slice(&part[i..]);
                *part = new_part;
            }

            let display = if best_str.starts_with(' ') || best_str.ends_with(' ') {
                format!("'{}'", best_str)
            } else {
                best_str
            };
            self.legend.push(format!("{} = {}", token, display));
        }

        parts
            .iter()
            .enumerate()
            .map(|(i, part)| {
                let mut s = part
                    .iter()
                    .map(|&id| id_to_str[id as usize].as_str())
                    .collect::<String>();
                if i < seps.len() {
                    s.push_str(seps[i]);
                }
                s
            })
            .collect()
    }

    fn process_templates(
        &mut self,
        templates: &HashMap<String, Vec<(usize, String)>>,
        lines: &mut [String],
    ) {
        let mut scores: Vec<_> = templates
            .iter()
            .map(|(t, m)| (t.clone(), m.len(), calc_savings(m.len(), t.len())))
            .filter(|&(_, count, sav)| sav > 0 || count >= MACRO_MIN_COUNT)
            .collect();

        // Deterministic: longest template first, then most matches, then the
        // template text (a final, fully-deterministic tie-break).
        scores.sort_by(|a, b| {
            b.0.len()
                .cmp(&a.0.len())
                .then(b.1.cmp(&a.1))
                .then_with(|| a.0.cmp(&b.0))
        });
        let mut templated = vec![false; lines.len()];

        for (template, _, _) in scores {
            if let Some(matches) = templates.get(&template) {
                let valid: Vec<_> = matches.iter().filter(|(i, _)| !templated[*i]).collect();
                if valid.len() > 1 {
                    let macro_tag = self.get_macro_token();
                    self.legend.push(format!("{} = {}", macro_tag, template));

                    for (i, var) in valid {
                        lines[*i] = format!("{}:{}", macro_tag, var);
                        templated[*i] = true;
                    }
                }
            }
        }
    }

    fn run_macro_templating(&mut self, text: String) -> String {
        let mut lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).collect();
        let mut templates: HashMap<String, Vec<(usize, String)>> = HashMap::new();

        for (i, line) in lines.iter().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            for mat in RE_TAGS.find_iter(line) {
                let mut template = line.clone();
                template.replace_range(mat.start()..mat.end(), "@");
                if template.len() >= MACRO_MIN_TEMPLATE_LEN {
                    templates
                        .entry(template)
                        .or_default()
                        .push((i, mat.as_str().to_string()));
                }
            }
        }

        self.process_templates(&templates, &mut lines);
        lines.join("\n")
    }

    fn deduplicate_lines(&self, text: &str) -> Vec<String> {
        let (mut final_lines, mut dup_count) = (Vec::new(), 0);
        let mut last_line: Cow<'_, str> = Cow::Borrowed("");
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }

            let line_no_time = RE_NO_TIME.replace(line, "");
            if line_no_time == last_line && !line_no_time.is_empty() {
                dup_count += 1;
                continue;
            }

            if dup_count > 0 {
                final_lines.push(format!("    ... x{}", dup_count + 1));
                dup_count = 0;
            }
            final_lines.push(line.to_string());
            last_line = line_no_time;
        }
        if dup_count > 0 {
            final_lines.push(format!("    ... x{}", dup_count + 1));
        }
        final_lines
    }

    /// Run the full pipeline. Returns the compressed *body* only — the legend is
    /// accumulated on `self.legend` and exposed separately by `squeeze()`.
    /// (Rather than prepending a `--- LEGEND ---` / `--- LOGS ---` text block,
    /// the caller decides how to present the legend.)
    fn compress(&mut self, mut text: String) -> String {
        if text.trim().is_empty() {
            return text;
        }

        text = RE_ZEROS_1.replace_all(&text, "$1").into_owned();
        text = RE_ZEROS_2.replace_all(&text, "$1").into_owned();
        text = RE_ZEROS_3.replace_all(&text, "${1}${2}").into_owned();
        text = RE_ZEROS_4.replace_all(&text, "${1}${2}").into_owned();

        // Most-frequent ISO8601 prefix -> #T#. Determinism: when two prefixes
        // tie on frequency, prefer the lexicographically smaller (a bare
        // `max_by_key` would keep an arbitrary one).
        if let Some(best_ts) = RE_TS
            .find_iter(&text)
            .fold(HashMap::new(), |mut acc, m| {
                *acc.entry(m.as_str()).or_insert(0) += 1;
                acc
            })
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
            .map(|(ts, _)| ts.to_string())
        {
            text = text.replace(&best_ts, "#T#");
            self.legend.push(format!("#T# = {}", best_ts));
        }

        self.replace_frequent(&mut text, &RE_COMP);
        self.replace_frequent(&mut text, &RE_KEYS);

        text = self.run_bpe(text, BPE_MAX_ITERATIONS, NormalBpe);
        text = self.run_bpe(text, BPE_MAX_ITERATIONS, MetaBpe);
        text = self.run_macro_templating(text);

        self.deduplicate_lines(&text).join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-expand a squeezed body using its legend, reversing every *reversible*
    /// stage. Applied last-assigned-first (reverse legend order) so nested tags
    /// unfold correctly (meta `!n!` -> exposes `#n#` -> expands). Macro `&n`
    /// entries are template-based (`@` placeholder + `&n:val` reference); all
    /// others are plain string substitutions.
    ///
    /// This reverses stages 2-6. It cannot recover the two intentionally lossy
    /// stages (leading-zero trim, line dedup), so the round-trip samples below
    /// are constructed to avoid triggering those.
    fn expand(body: &str, legend: &[(String, String)]) -> String {
        let mut text = body.to_string();
        for (tag, value) in legend.iter().rev() {
            if tag.starts_with('&') {
                // Macro: `&n:CAPTURE` -> `value` with `@` substituted by CAPTURE.
                // The macro-templating stage (`process_templates`) replaces a
                // whole line with `&n:<tag>`, so CAPTURE is exactly one `#…#` or
                // `!…!` token. We match the macro tag plus its colon plus that
                // one token.
                let re = Regex::new(&format!(
                    r"{}:(#(?:T|[0-9a-zA-Z]+)#|![0-9a-zA-Z]+!)",
                    regex::escape(tag)
                ))
                .unwrap();
                text = re
                    .replace_all(&text, |caps: &regex::Captures| {
                        value.replacen('@', &caps[1], 1)
                    })
                    .into_owned();
            } else {
                // Plain tag. The display value may be quoted if it had leading/
                // trailing spaces (`'...'`); unwrap that for the real substitution.
                let real = if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
                    &value[1..value.len() - 1]
                } else {
                    value.as_str()
                };
                text = text.replace(tag, real);
            }
        }
        text
    }

    #[test]
    fn empty_input_is_empty() {
        let s = squeeze("");
        assert_eq!(s.body, "");
        assert!(s.legend.is_empty());

        let s2 = squeeze("   \n\t\n  ");
        assert!(s2.legend.is_empty());
        // whitespace-only short-circuits and returns the input untouched
        assert_eq!(s2.body, "   \n\t\n  ");
    }

    #[test]
    fn tiny_nonrepetitive_input() {
        // A single unique line: nothing repeats, so no legend and the body is
        // just the (trimmed/deduplicated) line. Reconstruct it exactly.
        let input = "the quick brown fox jumps";
        let s = squeeze(input);
        assert!(s.legend.is_empty(), "no repetition -> no legend");
        assert_eq!(s.body, input);
        assert_eq!(expand(&s.body, &s.legend), input);
    }

    #[test]
    fn repetitive_log_shrinks_and_round_trips() {
        // Repetitive ISO8601-timestamped component logs. No leading-zero
        // sequences, no exact duplicate lines (each carries a distinct hwnd),
        // no trailing whitespace, no blank lines — so only the *reversible*
        // stages fire and the round-trip is exact.
        let input = "\
2026-05-30T11:54:19.557 [WinFocusMonitor] focus changed hwnd=11 title=alpha
2026-05-30T11:54:19.602 [WinFocusMonitor] focus changed hwnd=12 title=bravo
2026-05-30T11:54:19.640 [WinFocusMonitor] focus changed hwnd=13 title=charlie
2026-05-30T11:54:19.688 [WinFocusMonitor] focus changed hwnd=14 title=delta
2026-05-30T11:54:19.701 [WinFocusMonitor] focus changed hwnd=15 title=echo
2026-05-30T11:54:19.744 [WinFocusMonitor] focus changed hwnd=16 title=foxtrot
2026-05-30T11:54:19.780 [WinFocusMonitor] focus changed hwnd=17 title=golf
2026-05-30T11:54:19.822 [WinFocusMonitor] focus changed hwnd=18 title=hotel";

        let s = squeeze(input);

        assert!(
            !s.legend.is_empty(),
            "a repetitive log must produce legend entries"
        );
        assert!(
            s.body.len() < input.len(),
            "compressed body ({}) should be smaller than input ({})",
            s.body.len(),
            input.len()
        );

        let restored = expand(&s.body, &s.legend);
        assert_eq!(
            restored, input,
            "legend round-trip must reconstruct the input exactly"
        );
    }

    #[test]
    fn legend_entries_split_cleanly() {
        // A value that itself contains " = " must be preserved on the value
        // side (split only on the first separator).
        let (tag, val) = split_legend_entry("#0# = a = b".to_string());
        assert_eq!(tag, "#0#");
        assert_eq!(val, "a = b");
    }

    #[test]
    fn replace_frequent_does_not_corrupt_substring_patterns() {
        let input = "\
[WinFocus] changed
[WinFocusMonitor] changed
[WinFocus] changed
[WinFocusMonitor] changed";
        let mut text = input.to_string();
        let mut c = Compressor::new();

        c.replace_frequent(&mut text, &RE_COMP);

        let legend: Vec<_> = c.legend.into_iter().map(split_legend_entry).collect();
        let restored = expand(&text, &legend);
        assert_eq!(restored, input);
        assert_eq!(legend.len(), 2, "both component names should be replaced");
    }
}
