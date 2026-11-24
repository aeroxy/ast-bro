//! WordPiece tokenization as potion-code-16M's `tokenizer.json` describes it,
//! reimplemented so search no longer needs the HuggingFace `tokenizers`
//! crate, or the Oniguruma C library that crate builds.
//!
//! [`WordPiece::encode`] reproduces `tokenizers` 0.21's
//! `encode(text, add_special_tokens = false)` id for id, which a parity test
//! against that crate holds it to (`tokenizers` stays a dev-dependency):
//!
//! 1. Added tokens with `normalized: false` (`[UNK]`) are cut out of the raw
//!    text wherever they occur.
//! 2. Each remaining segment is normalized as `BertNormalizer` does: control
//!    characters dropped, other whitespace made a space, CJK ideographs padded
//!    with spaces, NFD with non-spacing marks removed, then lowercased.
//! 3. Added tokens with `normalized: true` (`[PAD]`, matched as `[pad]`) are
//!    cut out of the normalized text, honouring `single_word`, `lstrip` and
//!    `rstrip`.
//! 4. The rest splits on whitespace and around every punctuation character
//!    (`BertPreTokenizer`), and each piece becomes WordPiece tokens: the
//!    longest vocabulary match from the left, `##` on continuations, and one
//!    `[UNK]` for a word with an unmatchable position or more than
//!    `max_input_chars_per_word` characters.
//!
//! The Unicode tables come from the crates `tokenizers` uses
//! (`unicode_categories`, `unicode-normalization-alignments`), and the
//! added-token matching from the same `aho-corasick` settings and `regex`
//! patterns, so the two agree on every character rather than on most.
//! [`WordPiece::load`] refuses a `tokenizer.json` describing any other
//! pipeline, a post-processor included, so another model cannot be tokenized
//! wrong silently. Truncation and padding are not part of the pipeline here:
//! the embedder applies its own window (`MAX_EMBED_TOKENS`) and never pads.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::LazyLock;
use unicode_categories::UnicodeCategories;
use unicode_normalization_alignments::UnicodeNormalization;

/// A loaded WordPiece tokenizer.
pub struct WordPiece {
  vocab: HashMap<String, u32>,
  unk_id: u32,
  /// Marks a continuation piece in the vocabulary: `##`.
  prefix: String,
  /// A word longer than this, in characters, is one `[UNK]`.
  max_chars: usize,
  /// Added tokens matched in the raw text (`normalized: false`).
  raw: AddedTokens,
  /// Added tokens matched in the normalized text, by normalized content.
  normalized: AddedTokens,
}

struct AddedToken {
  id: u32,
  single_word: bool,
  lstrip: bool,
  rstrip: bool,
}

/// Added tokens and the automaton that finds them.
struct AddedTokens {
  tokens: Vec<AddedToken>,
  matcher: Option<AhoCorasick>,
}

#[derive(Deserialize)]
struct TokenizerJson {
  /// `tokenizers` accepts an absent version but rejects any explicit one
  /// other than "1.0" before it reads the pipeline.
  #[serde(default)]
  version: Option<String>,
  #[serde(default)]
  added_tokens: Vec<AddedTokenJson>,
  normalizer: Option<Value>,
  pre_tokenizer: Option<Value>,
  /// Must be null: a post-processor can change the ids even with special
  /// tokens off, since a template keeps every `$A` it names.
  #[serde(default)]
  post_processor: Option<Value>,
  model: ModelJson,
}

#[derive(Deserialize)]
struct AddedTokenJson {
  id: u32,
  content: String,
  single_word: bool,
  lstrip: bool,
  rstrip: bool,
  normalized: bool,
}

#[derive(Deserialize)]
struct ModelJson {
  #[serde(rename = "type")]
  kind: String,
  unk_token: String,
  continuing_subword_prefix: String,
  max_input_chars_per_word: usize,
  vocab: HashMap<String, u32>,
}

/// Whether the exclusive upper bound of `id` fits in a `usize` whose maximum
/// is `usize_max`. Split out so the 32-bit case is testable on a 64-bit host.
fn id_fits_usize(id: u32, usize_max: u64) -> bool {
  u64::from(id) < usize_max
}

// The patterns `tokenizers` uses to place a `single_word` token and to strip
// beside an `lstrip`/`rstrip` one.
static STARTS_WITH_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\w").unwrap());
static ENDS_WITH_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\w$").unwrap());
static SPACE_AT_START: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*").unwrap());
static SPACE_AT_END: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*$").unwrap());

impl WordPiece {
  /// Load the tokenizer a model's `tokenizer.json` describes.
  pub fn load(path: &Path) -> io::Result<Self> {
    Self::from_json(&std::fs::read(path)?)
  }

  /// Build a tokenizer from the bytes of a `tokenizer.json`, refusing any
  /// pipeline other than the one this module implements.
  pub fn from_json(bytes: &[u8]) -> io::Result<Self> {
    let invalid =
      |msg: String| io::Error::new(io::ErrorKind::InvalidData, format!("tokenizer.json: {msg}"));
    let json: TokenizerJson = serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))?;
    if let Some(version) = &json.version {
      if version != "1.0" {
        return Err(invalid(format!(
          "unsupported tokenizer version {version:?}"
        )));
      }
    }
    check_normalizer(json.normalizer.as_ref()).map_err(invalid)?;
    let pre_tokenizer = json.pre_tokenizer.as_ref().and_then(|p| p.get("type"));
    if pre_tokenizer.and_then(Value::as_str) != Some("BertPreTokenizer") {
      return Err(invalid(format!(
        "pre_tokenizer {} is not the BertPreTokenizer this tokenizer implements",
        json.pre_tokenizer.unwrap_or(Value::Null)
      )));
    }
    if let Some(processor) = &json.post_processor {
      return Err(invalid(format!(
        "post_processor {processor} is not supported; only null is, since a \
         post-processor can change the ids even without special tokens"
      )));
    }
    if json.model.kind != "WordPiece" {
      return Err(invalid(format!(
        "model type {} is not WordPiece",
        json.model.kind
      )));
    }
    let unk_id = *json.model.vocab.get(&json.model.unk_token).ok_or_else(|| {
      invalid(format!(
        "unk_token {:?} is not in the vocabulary",
        json.model.unk_token
      ))
    })?;
    // `id_count` computes `id as usize + 1`; on a 32-bit target u32::MAX
    // would overflow that. Added tokens must agree with the vocabulary, so
    // its largest id bounds both.
    let largest_id = json.model.vocab.values().copied().max().unwrap_or(0);
    if !id_fits_usize(largest_id, usize::MAX as u64) {
      return Err(invalid(format!(
        "token id {largest_id} has no exclusive bound in usize"
      )));
    }

    // `tokenizers` gives an added token its id from the vocabulary, warning
    // and moving on when the two disagree; only agreeing ids are supported.
    // It also looks a match's `single_word`, `lstrip` and `rstrip` up by id,
    // so of two tokens sharing an id, both would match with the later one's
    // flags; a shared id is refused.
    let mut ids: HashMap<u32, &str> = HashMap::new();
    for token in &json.added_tokens {
      match json.model.vocab.get(&token.content) {
        Some(&id) if id == token.id => {}
        Some(&id) => {
          return Err(invalid(format!(
            "added token {:?} has id {} in added_tokens but {id} in the vocabulary",
            token.content, token.id
          )))
        }
        None => {
          return Err(invalid(format!(
            "added token {:?} is not in the vocabulary",
            token.content
          )))
        }
      }
      if let Some(other) = ids.insert(token.id, &token.content) {
        return Err(invalid(format!(
          "added tokens {other:?} and {:?} share id {}",
          token.content, token.id
        )));
      }
    }

    let (raw, normalized): (Vec<_>, Vec<_>) =
      json.added_tokens.into_iter().partition(|t| !t.normalized);
    let raw = AddedTokens::new(raw.into_iter().map(|t| (t.content.clone(), t)))?;
    let normalized = AddedTokens::new(normalized.into_iter().map(|t| (normalize(&t.content), t)))?;

    Ok(Self {
      vocab: json.model.vocab,
      unk_id,
      prefix: json.model.continuing_subword_prefix,
      max_chars: json.model.max_input_chars_per_word,
      raw,
      normalized,
    })
  }

  /// One past the largest id `encode` can return, so the embedder can check
  /// it has a row for every one.
  pub fn id_count(&self) -> usize {
    let vocab = self.vocab.values().copied().max();
    let added = (self.raw.tokens.iter())
      .chain(&self.normalized.tokens)
      .map(|t| t.id)
      .max();
    vocab.max(added).map_or(0, |id| id as usize + 1)
  }

  /// Token ids for `text`, stopping once `limit` are produced. Tokenization
  /// never looks back, so these are the first `limit` ids `tokenizers` would
  /// give for the whole text; a long chunk just skips work it would discard.
  pub fn encode(&self, text: &str, limit: usize) -> Vec<u32> {
    let mut ids = Vec::new();
    let mut segments = Vec::new();
    self.raw.split(text, &mut segments);
    for (segment, id) in segments {
      if ids.len() >= limit {
        break;
      }
      if let Some(id) = id {
        ids.push(id);
        continue;
      }
      let normalized = normalize(segment);
      let mut pieces = Vec::new();
      self.normalized.split(&normalized, &mut pieces);
      for (piece, id) in pieces {
        if ids.len() >= limit {
          break;
        }
        match id {
          Some(id) => ids.push(id),
          None => self.encode_words(piece, &mut ids, limit),
        }
      }
    }
    ids.truncate(limit);
    ids
  }

  /// `BertPreTokenizer`, then WordPiece on each piece: whitespace separates
  /// words and is dropped, and every punctuation character is a word of its
  /// own.
  fn encode_words(&self, text: &str, ids: &mut Vec<u32>, limit: usize) {
    let mut word_start = None;
    for (i, c) in text.char_indices() {
      let space = c.is_whitespace();
      if space || is_bert_punc(c) {
        if let Some(start) = word_start.take() {
          if ids.len() >= limit {
            return;
          }
          self.wordpiece(&text[start..i], ids);
        }
        if !space {
          if ids.len() >= limit {
            return;
          }
          self.wordpiece(&text[i..i + c.len_utf8()], ids);
        }
      } else if word_start.is_none() {
        word_start = Some(i);
      }
    }
    if let Some(start) = word_start {
      if ids.len() < limit {
        self.wordpiece(&text[start..], ids);
      }
    }
  }

  /// One word: the longest vocabulary match from the left, then the longest
  /// `##` continuation from there, and so on. A position nothing matches
  /// makes the whole word one `[UNK]`.
  fn wordpiece(&self, word: &str, ids: &mut Vec<u32>) {
    if word.chars().count() > self.max_chars {
      ids.push(self.unk_id);
      return;
    }
    let first = ids.len();
    let mut key = String::new();
    let mut start = 0;
    while start < word.len() {
      let mut end = word.len();
      let mut found = None;
      while start < end {
        let piece = &word[start..end];
        let id = if start == 0 {
          self.vocab.get(piece)
        } else {
          key.clear();
          key.push_str(&self.prefix);
          key.push_str(piece);
          self.vocab.get(key.as_str())
        };
        if let Some(&id) = id {
          found = Some(id);
          break;
        }
        end -= piece.chars().next_back().map_or(1, char::len_utf8);
      }
      match found {
        Some(id) => {
          ids.push(id);
          start = end;
        }
        None => {
          ids.truncate(first);
          ids.push(self.unk_id);
          return;
        }
      }
    }
  }
}

impl AddedTokens {
  /// Index the added tokens by the pattern each is matched as. A pattern
  /// that is empty, or starts or ends with whitespace, is refused: a match
  /// could then begin inside whitespace an earlier `rstrip` consumed, where
  /// `tokenizers` builds a reversed range, and with whitespace-free edges it
  /// never can.
  fn new(tokens: impl Iterator<Item = (String, AddedTokenJson)>) -> io::Result<Self> {
    let mut patterns: Vec<String> = Vec::new();
    let mut contents: Vec<String> = Vec::new();
    let mut added = Vec::new();
    for (pattern, t) in tokens {
      if pattern.is_empty()
        || pattern.starts_with(char::is_whitespace)
        || pattern.ends_with(char::is_whitespace)
      {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          format!(
            "tokenizer.json: added token {:?} is empty or starts or ends with whitespace, \
             which this tokenizer does not match",
            t.content
          ),
        ));
      }
      // Two tokens matched as the same pattern tie on every match.
      // `tokenizers` gives the match to whichever it lists first, and it
      // lists special tokens before the rest, so file order alone would
      // not reproduce its choice.
      if let Some(other) = patterns.iter().position(|p| *p == pattern) {
        return Err(io::Error::new(
          io::ErrorKind::InvalidData,
          format!(
            "tokenizer.json: added tokens {:?} and {:?} both match as {pattern:?}",
            contents[other], t.content
          ),
        ));
      }
      contents.push(t.content.clone());
      patterns.push(pattern);
      added.push(AddedToken {
        id: t.id,
        single_word: t.single_word,
        lstrip: t.lstrip,
        rstrip: t.rstrip,
      });
    }
    let tokens = added;
    let matcher = if patterns.is_empty() {
      None
    } else {
      let built = AhoCorasickBuilder::new()
        .match_kind(MatchKind::LeftmostLongest)
        .build(&patterns)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("tokenizer.json: {e}")))?;
      Some(built)
    };
    Ok(Self { tokens, matcher })
  }

  /// Split `text` around the added tokens in it, as `tokenizers`'
  /// `AddedVocabulary::find_matches` does: leftmost-longest matches without
  /// overlap; a `single_word` token is passed over when a word character
  /// touches it; `lstrip` and `rstrip` widen a match over the whitespace
  /// beside it.
  fn split<'a>(&self, text: &'a str, out: &mut Vec<(&'a str, Option<u32>)>) {
    let Some(matcher) = &self.matcher else {
      if !text.is_empty() {
        out.push((text, None));
      }
      return;
    };
    let mut done = 0;
    for m in matcher.find_iter(text) {
      let token = &self.tokens[m.pattern().as_usize()];
      let (mut start, mut stop) = (m.start(), m.end());
      if token.single_word {
        let space_before = start == 0 || !ENDS_WITH_WORD.is_match(&text[..start]);
        let space_after = stop == text.len() || !STARTS_WITH_WORD.is_match(&text[stop..]);
        if !space_before || !space_after {
          continue;
        }
      }
      if token.lstrip {
        let stripped = SPACE_AT_END
          .find(&text[..start])
          .map_or(start, |s| s.start());
        start = stripped.max(done);
      }
      if token.rstrip {
        stop += SPACE_AT_START.find(&text[stop..]).map_or(0, |s| s.end());
      }
      if done < start {
        out.push((&text[done..start], None));
      }
      out.push((&text[start..stop], Some(token.id)));
      done = stop;
    }
    if done < text.len() {
      out.push((&text[done..], None));
    }
  }
}

/// The only normalizer this module implements: `BertNormalizer` with
/// cleaning, CJK padding, accent stripping and lowercasing all on.
fn check_normalizer(normalizer: Option<&Value>) -> Result<(), String> {
  let Some(n) = normalizer else {
    return Err("no normalizer; BertNormalizer expected".to_string());
  };
  let on = |key: &str| n.get(key).and_then(Value::as_bool) == Some(true);
  let lowercase = on("lowercase");
  // `strip_accents: null` follows `lowercase`.
  let strip_accents = match n.get("strip_accents") {
    None | Some(Value::Null) => lowercase,
    Some(v) => v.as_bool() == Some(true),
  };
  let bert = n.get("type").and_then(Value::as_str) == Some("BertNormalizer");
  if bert && on("clean_text") && on("handle_chinese_chars") && lowercase && strip_accents {
    Ok(())
  } else {
    Err(format!(
      "normalizer {n} is not the BertNormalizer this tokenizer implements \
       (clean_text, handle_chinese_chars, strip_accents and lowercase all on)"
    ))
  }
}

/// `BertNormalizer` with every option on: drop control characters and turn
/// other whitespace into a space, pad CJK ideographs with spaces, then NFD
/// without non-spacing marks, lowercased one character at a time.
fn normalize(text: &str) -> String {
  let mut cleaned = String::with_capacity(text.len());
  for c in text.chars() {
    if c == '\0' || c == '\u{fffd}' || is_control(c) {
      continue;
    }
    let c = if is_whitespace(c) { ' ' } else { c };
    if is_chinese_char(c) {
      cleaned.push(' ');
      cleaned.push(c);
      cleaned.push(' ');
    } else {
      cleaned.push(c);
    }
  }
  // ASCII has no decompositions and no marks, and lowercases to ASCII.
  if cleaned.is_ascii() {
    cleaned.make_ascii_lowercase();
    return cleaned;
  }
  let mut out = String::with_capacity(cleaned.len());
  for (c, _) in cleaned.as_str().nfd() {
    if !c.is_mark_nonspacing() {
      out.extend(c.to_lowercase());
    }
  }
  out
}

/// `tokenizers`' BERT whitespace test: tab, newline and carriage return,
/// plus Unicode White_Space.
fn is_whitespace(c: char) -> bool {
  matches!(c, '\t' | '\n' | '\r') || c.is_whitespace()
}

/// `tokenizers`' BERT control test: Cc, Cf and Co as `unicode_categories`
/// defines them, except the three whitespace controls.
fn is_control(c: char) -> bool {
  !matches!(c, '\t' | '\n' | '\r') && c.is_other()
}

/// The CJK Unified Ideographs blocks `BertNormalizer` pads with spaces.
fn is_chinese_char(c: char) -> bool {
  matches!(
    c as u32,
    0x4E00..=0x9FFF
      | 0x3400..=0x4DBF
      | 0x20000..=0x2A6DF
      | 0x2A700..=0x2B73F
      | 0x2B740..=0x2B81F
      | 0x2B920..=0x2CEAF
      | 0xF900..=0xFAFF
      | 0x2F800..=0x2FA1F
  )
}

/// `BertPreTokenizer`'s punctuation: ASCII punctuation, or any Unicode P*.
fn is_bert_punc(c: char) -> bool {
  c.is_ascii_punctuation() || c.is_punctuation()
}

#[cfg(test)]
mod tests {
  use super::*;
  use rayon::prelude::*;
  use std::collections::HashSet;
  use std::path::PathBuf;

  /// Strings that exercise every step: accents and decompositions, case
  /// mappings that expand, CJK padding, every class of whitespace and
  /// control character, both special tokens in and out of word context,
  /// over-long words, and punctuation from beyond ASCII.
  const CASES: &[&str] = &[
    "Hello, World!",
    "hello world unable tokenizer",
    "",
    "   ",
    "x[UNK]y",
    "[UNK]",
    "[UNK][UNK] [unk]",
    "a [PAD] b",
    "a[PAD]b",
    "  [PAD]  tail",
    "[pad]",
    "x [Pad] y",
    "[PAD][PAD]",
    "x[pad] [pad]",
    "Hello[UNK]World [PAD]",
    "Café naïve résumé",
    "e\u{301} vs é",
    "İstanbul ISTANBUL",
    "ΣΑΣ σας",
    "Straße STRASSE",
    "中文字 mixed中文",
    "tab\tnew\nline\rcr",
    "ctl\u{7}\u{b}\u{c}\u{1f}x",
    "nbsp\u{a0}here\u{2028}and\u{3000}there",
    "x\u{85}y",
    "zero\u{200b}width\u{feff}",
    "\u{fffd}x\u{0}y",
    "private\u{e000}use",
    "emoji 😀 ok",
    "ﬁne ligature",
    "##",
    "## ##a",
    "foo_bar.baz(qux) -> Option<u32>;",
    "x-y–z—w ¿¡ «quoted»",
    "「quote」（fullwidth）",
    "under_score__ __init__",
    "fn main() { println!(\"hi\"); }",
  ];

  /// A `tokenizer.json` with potion-code-16M's pipeline and token flags over
  /// a small vocabulary: printable ASCII, `##` continuations, a few words and
  /// pieces, CJK, and both special tokens.
  fn fixture() -> String {
    let mut seen = HashSet::new();
    let mut tokens: Vec<String> = Vec::new();
    let mut add = |t: String| {
      if seen.insert(t.clone()) {
        tokens.push(t);
      }
    };
    add("[PAD]".into());
    add("[UNK]".into());
    for b in 0x21u8..=0x7e {
      add((b as char).to_string());
    }
    for c in ('a'..='z').chain('0'..='9') {
      add(format!("##{c}"));
    }
    for w in [
      "hello", "world", "##lo", "##ld", "un", "##able", "token", "##izer", "caf", "fn", "main",
      "istanbul", "##_", "中", "文", "σ", "ς", "ß", "##ß", "naive", "resume", "fine", "##ne", "ﬁ",
      "pad", "unk",
    ] {
      add(w.into());
    }
    let vocab: Vec<String> = tokens
      .iter()
      .enumerate()
      .map(|(i, t)| format!("{}:{i}", serde_json::to_string(t).unwrap()))
      .collect();
    format!(
      r###"{{"version":"1.0","truncation":null,"padding":null,
      "added_tokens":[
        {{"id":0,"content":"[PAD]","single_word":true,"lstrip":true,"rstrip":true,"normalized":true,"special":true}},
        {{"id":1,"content":"[UNK]","single_word":false,"lstrip":false,"rstrip":false,"normalized":false,"special":true}}],
      "normalizer":{{"type":"BertNormalizer","clean_text":true,"handle_chinese_chars":true,"strip_accents":null,"lowercase":true}},
      "pre_tokenizer":{{"type":"BertPreTokenizer"}},
      "post_processor":null,
      "decoder":{{"type":"WordPiece","prefix":"##","cleanup":true}},
      "model":{{"type":"WordPiece","unk_token":"[UNK]","continuing_subword_prefix":"##",
        "max_input_chars_per_word":100,"vocab":{{{}}}}}}}"###,
      vocab.join(",")
    )
  }

  fn reference(json: &str) -> tokenizers::Tokenizer {
    json.parse().expect("the fixture is a valid tokenizer.json")
  }

  #[test]
  fn matches_tokenizers_id_for_id() {
    let json = fixture();
    let ours = WordPiece::from_json(json.as_bytes()).unwrap();
    let theirs = reference(&json);
    let long_word = "a".repeat(100);
    let too_long = "a".repeat(101);
    let mut cases: Vec<&str> = CASES.to_vec();
    cases.extend([long_word.as_str(), too_long.as_str()]);
    for case in cases {
      let want = theirs.encode(case, false).unwrap().get_ids().to_vec();
      assert_eq!(ours.encode(case, usize::MAX), want, "{case:?}");
    }
  }

  #[test]
  fn a_limit_keeps_the_head_of_the_full_encoding() {
    let ours = WordPiece::from_json(fixture().as_bytes()).unwrap();
    for case in CASES {
      let full = ours.encode(case, usize::MAX);
      for limit in [0, 1, 2, 3, 7] {
        let head = &full[..full.len().min(limit)];
        assert_eq!(
          ours.encode(case, limit),
          head,
          "{case:?} limited to {limit}"
        );
      }
    }
  }

  /// A `TemplateProcessing` post-processor whose single template is `$A $A`.
  const REPEAT_A: &str = r#""post_processor":{"type":"TemplateProcessing",
    "single":[{"Sequence":{"id":"A","type_id":0}},{"Sequence":{"id":"A","type_id":0}}],
    "pair":[{"Sequence":{"id":"A","type_id":0}},{"Sequence":{"id":"B","type_id":1}}],
    "special_tokens":{}}"#;

  /// Why a post-processor is refused rather than ignored: `tokenizers` keeps
  /// every `$A` a template names even with special tokens off, so this one
  /// doubles the ids.
  #[test]
  fn a_template_post_processor_changes_the_ids_without_special_tokens() {
    let json = fixture().replacen(r#""post_processor":null"#, REPEAT_A, 1);
    let once = reference(&fixture())
      .encode("hello world", false)
      .unwrap()
      .get_ids()
      .to_vec();
    let twice = reference(&json)
      .encode("hello world", false)
      .unwrap()
      .get_ids()
      .to_vec();
    assert_eq!(twice, [once.clone(), once].concat());
  }

  #[test]
  fn load_refuses_a_pipeline_it_does_not_implement() {
    let json = fixture();
    for (from, to, named) in [
      (r#""lowercase":true"#, r#""lowercase":false"#, "normalizer"),
      (
        r#""type":"BertPreTokenizer""#,
        r#""type":"Whitespace""#,
        "pre_tokenizer",
      ),
      (
        r#""model":{"type":"WordPiece""#,
        r#""model":{"type":"BPE""#,
        "WordPiece",
      ),
      (
        r#""unk_token":"[UNK]""#,
        r#""unk_token":"[NOPE]""#,
        "unk_token",
      ),
      (r#""post_processor":null"#, REPEAT_A, "post_processor"),
      (
        r#"{"id":1,"content":"[UNK]""#,
        r#"{"id":2,"content":"[UNK]""#,
        "in added_tokens but 1 in the vocabulary",
      ),
    ] {
      let changed = json.replacen(from, to, 1);
      assert_ne!(changed, json, "the fixture has no {from}");
      let err = match WordPiece::from_json(changed.as_bytes()) {
        Err(e) => e,
        Ok(_) => panic!("changing {from} must be refused"),
      };
      assert_eq!(err.kind(), io::ErrorKind::InvalidData);
      assert!(err.to_string().contains(named), "{err}");
    }
  }

  /// A raw `x` that strips the whitespace after it, and a raw token that is
  /// one space: `x  tail` used to give the second match a start past its
  /// end, and slicing panicked. A whitespace-edged added token is refused
  /// at load instead.
  #[test]
  fn load_refuses_an_added_token_edged_with_whitespace() {
    let json = fixture()
      .replacen(r#""vocab":{"#, r#""vocab":{" ":900,"#, 1)
      .replacen(
        r#""added_tokens":["#,
        r#""added_tokens":[
          {"id":89,"content":"x","single_word":false,"lstrip":false,"rstrip":true,"normalized":false,"special":true},
          {"id":900,"content":" ","single_word":false,"lstrip":true,"rstrip":false,"normalized":false,"special":true},"#,
        1,
      );
    let err = match WordPiece::from_json(json.as_bytes()) {
      Err(e) => e,
      Ok(_) => panic!("a whitespace-edged added token must be refused"),
    };
    assert!(
      err.to_string().contains("starts or ends with whitespace"),
      "{err}"
    );
  }

  /// `HELLO` and `Hello` both normalize to `hello`, so as normalized added
  /// tokens they tie on every match. `tokenizers` lists special tokens
  /// before the rest, so the earlier one in the file need not win; the pair
  /// is refused at load instead.
  #[test]
  fn load_refuses_two_added_tokens_that_match_alike() {
    let json = fixture()
      .replacen(r#""vocab":{"#, r#""vocab":{"HELLO":901,"Hello":902,"#, 1)
      .replacen(
        r#""added_tokens":["#,
        r#""added_tokens":[
          {"id":901,"content":"HELLO","single_word":false,"lstrip":false,"rstrip":false,"normalized":true,"special":false},
          {"id":902,"content":"Hello","single_word":false,"lstrip":false,"rstrip":false,"normalized":true,"special":true},"#,
        1,
      );
    let err = match WordPiece::from_json(json.as_bytes()) {
      Err(e) => e,
      Ok(_) => panic!("two added tokens that normalize alike must be refused"),
    };
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(
      err
        .to_string()
        .contains(r#""HELLO" and "Hello" both match as "hello""#),
      "{err}"
    );
  }

  /// `QQ` and `ZZ` share id 901 with different `single_word` flags.
  /// `tokenizers` looks the flags up by id, so both match as `ZZ` does:
  /// `aQQb` stays one word there but splits around `QQ` here. A shared id
  /// is refused at load instead.
  #[test]
  fn load_refuses_two_added_tokens_that_share_an_id() {
    let json = fixture()
      .replacen(r#""vocab":{"#, r#""vocab":{"QQ":901,"ZZ":901,"#, 1)
      .replacen(
        r#""added_tokens":["#,
        r#""added_tokens":[
          {"id":901,"content":"QQ","single_word":false,"lstrip":false,"rstrip":false,"normalized":false,"special":false},
          {"id":901,"content":"ZZ","single_word":true,"lstrip":false,"rstrip":false,"normalized":false,"special":false},"#,
        1,
      );
    let err = match WordPiece::from_json(json.as_bytes()) {
      Err(e) => e,
      Ok(_) => panic!("two added tokens sharing an id must be refused"),
    };
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(
      err.to_string().contains(r#""QQ" and "ZZ" share id 901"#),
      "{err}"
    );
  }

  /// `tokenizers` rejects any explicit version but "1.0" before reading the
  /// pipeline; accepting one would let a candidate pass the native check and
  /// then fail to start the parity test.
  #[test]
  fn load_refuses_a_newer_tokenizer_version() {
    let json = fixture().replacen(r#""version":"1.0""#, r#""version":"2.0""#, 1);
    assert!(
      json.parse::<tokenizers::Tokenizer>().is_err(),
      "the reference refuses it too"
    );
    let err = match WordPiece::from_json(json.as_bytes()) {
      Err(e) => e,
      Ok(_) => panic!("a new tokenizer version must be refused"),
    };
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(
      err
        .to_string()
        .contains(r#"unsupported tokenizer version "2.0""#),
      "{err}"
    );
  }

  /// An absent version is accepted by `tokenizers`, so it must be here too.
  #[test]
  fn load_accepts_a_versionless_tokenizer() {
    let json = fixture().replacen(r#""version":"1.0","#, "", 1);
    assert!(
      json.parse::<tokenizers::Tokenizer>().is_ok(),
      "the reference accepts it"
    );
    WordPiece::from_json(json.as_bytes())
      .unwrap_or_else(|e| panic!("a versionless tokenizer must load: {e}"));
  }

  /// `id_count` computes `id as usize + 1`. On a 32-bit target u32::MAX has
  /// no exclusive bound in usize; check both widths against the helper.
  #[test]
  fn token_ids_fit_an_exclusive_usize_bound() {
    assert!(id_fits_usize(u32::MAX, u64::MAX), "64-bit");
    assert!(!id_fits_usize(u32::MAX, u64::from(u32::MAX)), "32-bit");
    assert!(id_fits_usize(0, u64::from(u32::MAX)), "32-bit, ordinary id");
  }

  /// The real model's `tokenizer.json` against `tokenizers` over every text
  /// file under `AST_BRO_PARITY_DIR` (default: this crate's `src/`), each
  /// tokenized whole. `AST_BRO_PARITY_TOKENIZER` names another
  /// `tokenizer.json`, such as a candidate model's; without it the pinned
  /// model is used, downloaded on first run, so the test is ignored by
  /// default: `cargo test --lib wordpiece -- --ignored --nocapture`.
  #[test]
  #[ignore]
  fn network_matches_tokenizers_on_real_text() {
    use crate::search::download::{ensure_model, ModelInfo};
    let path = match std::env::var_os("AST_BRO_PARITY_TOKENIZER") {
      Some(path) => PathBuf::from(path),
      None => ensure_model(&ModelInfo::potion_code_16m())
        .expect("model download")
        .join("tokenizer.json"),
    };
    eprintln!("tokenizer: {}", path.display());
    let ours = WordPiece::load(&path).unwrap();
    let mut theirs = tokenizers::Tokenizer::from_file(&path).unwrap();
    theirs.with_truncation(None).unwrap();
    theirs.with_padding(None);

    let root = std::env::var_os("AST_BRO_PARITY_DIR")
      .map(PathBuf::from)
      .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"));
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in ignore::WalkBuilder::new(&root).build() {
      let entry = entry.unwrap_or_else(|e| panic!("walking {}: {e}", root.display()));
      if entry.file_type().is_some_and(|t| t.is_file()) {
        files.push(entry.into_path());
      }
    }
    let results: Vec<(usize, Option<String>)> = files
      .par_iter()
      .filter_map(|file| {
        let text = std::fs::read_to_string(file).ok()?;
        if text.len() > 2_000_000 {
          return None;
        }
        let want = theirs
          .encode(text.as_str(), false)
          .unwrap()
          .get_ids()
          .to_vec();
        let got = ours.encode(&text, usize::MAX);
        let mismatch = (got != want).then(|| {
          let at = got.iter().zip(&want).take_while(|(a, b)| a == b).count();
          format!(
            "{}: first difference at id {at} of {}",
            file.display(),
            want.len()
          )
        });
        Some((want.len(), mismatch))
      })
      .collect();
    let tokens: usize = results.iter().map(|(n, _)| n).sum();
    assert!(
      tokens > 0,
      "compared nothing under {}; is AST_BRO_PARITY_DIR right?",
      root.display()
    );
    let mismatches: Vec<&String> = results.iter().filter_map(|(_, m)| m.as_ref()).collect();
    eprintln!(
      "compared {} files, {tokens} tokens, under {}",
      results.len(),
      root.display()
    );
    assert!(
      mismatches.is_empty(),
      "{} of {} files differ:\n{}",
      mismatches.len(),
      results.len(),
      mismatches
        .iter()
        .take(10)
        .map(|m| m.as_str())
        .collect::<Vec<_>>()
        .join("\n")
    );
  }
}
