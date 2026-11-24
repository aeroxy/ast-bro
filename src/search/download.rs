//! Model file downloader with HuggingFace → hf-mirror.com fallback.
//!
//! The model is pinned: [`ModelInfo`] names the upstream commit and the
//! SHA-256 each file has there, so every install reads the same bytes. TLS
//! verification is off by default (see `build_client`), which makes the
//! pinned hashes the integrity check a download actually gets.
//!
//! Behaviour:
//! 1. If every cached file still has the size and mtime recorded when it was
//!    last verified, return immediately. Hashing the 64 MB of weights on every
//!    load took ~120 ms, most of a warm CLI search. A file whose size or mtime
//!    changed, or that has no record (placed by hand), is hashed and must
//!    match its pinned value.
//! 2. Otherwise probe `https://huggingface.co/<id>/resolve/<rev>/config.json`
//!    with a 3-second connect+TLS timeout. Probe success requires HTTP 200
//!    and a plausible content-length — defends against captive-portal
//!    redirects that 200 with HTML.
//! 3. On probe success, download from HuggingFace. Otherwise download from
//!    `https://hf-mirror.com/<id>/resolve/<rev>/<file>` (URL-rewrite mirror).
//! 4. Each file streams to a `.tmp`, is checked against its pinned SHA-256,
//!    and only then renamed into the cache dir. `manifest.json` records the
//!    hash plus the size and mtime it was verified at.
//!
//! Env overrides:
//! - `AST_BRO_MODEL_DIR` (or legacy `AST_OUTLINE_MODEL_DIR`) — replace the
//!   default cache root entirely
//! - `AST_BRO_MODEL_SOURCE=hf|hf-mirror|<base-url>` (or legacy
//!   `AST_OUTLINE_MODEL_SOURCE`) — skip the probe and force a specific source

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::hash::{BuildHasher, RandomState};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

const HF_BASE: &str = "https://huggingface.co";
const HF_MIRROR_BASE: &str = "https://hf-mirror.com";
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const PROBE_FILE: &str = "config.json";

/// Approximate size of `config.json` for the supported model. Used to detect
/// captive-portal probe responses that return 200 with an HTML login page.
/// Real config.json files for sentence-transformers / model2vec are 200-2000 B;
/// captive portals typically return >>10 KB of HTML.
const PROBE_REFERENCE_SIZE: u64 = 600;
const PROBE_SIZE_TOLERANCE: f64 = 5.0; // ±500%, generous — only catches obvious anomalies

/// One file of a pinned model, with the SHA-256 it has at
/// [`ModelInfo::revision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFile {
  pub name: &'static str,
  pub sha256: &'static str,
}

/// A model pinned to one upstream commit.
#[derive(Debug, Clone)]
pub struct ModelInfo {
  pub id: String,
  /// The commit files are fetched from (`resolve/<revision>/…`). Following
  /// `main` instead would let a re-upload change the model under existing
  /// indexes, and leave nothing to check a download against.
  pub revision: &'static str,
  pub files: Vec<ModelFile>,
}

impl ModelInfo {
  pub fn potion_code_16m() -> Self {
    Self {
      id: "minishlab/potion-code-16M".to_string(),
      // `main` as of 2026-11-23. These are the bytes `main` has served since
      // search shipped (2026-05), so pinning them re-downloads nothing.
      revision: "1b0ff71095656b23306542bbad34a09109673720",
      files: vec![
        ModelFile {
          name: "config.json",
          sha256: "edf07552b5d768d556ded176d19f3a34f25360548de3a246d226ce8e28647914",
        },
        ModelFile {
          name: "tokenizer.json",
          sha256: "8e84217af15e70e8127c855435fc3d8a4cd91d7bbe686f72e75f188118ec78ae",
        },
        ModelFile {
          name: "model.safetensors",
          sha256: "ca6159081a6e96cebe4ad878e5e8437bfccc761e8db16223370149cd2faa6c0b",
        },
      ],
    }
  }

  /// Where `file` lives under `base` at the pinned revision.
  fn url(&self, base: &str, file: &ModelFile) -> String {
    format!("{base}/{}/resolve/{}/{}", self.id, self.revision, file.name)
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source<'a> {
  HuggingFace,
  HfMirror,
  Custom(&'a str),
}

impl<'a> Source<'a> {
  fn base_url(self) -> &'a str {
    match self {
      Source::HuggingFace => HF_BASE,
      Source::HfMirror => HF_MIRROR_BASE,
      Source::Custom(url) => url,
    }
  }

  fn label(self) -> &'static str {
    match self {
      Source::HuggingFace => "hf",
      Source::HfMirror => "hf-mirror",
      Source::Custom(_) => "custom",
    }
  }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
  /// Map from filename -> hex sha256 string.
  sha256: HashMap<String, String>,
  /// Which source populated this cache: "hf", "hf-mirror", "custom", or
  /// "local" for files placed by hand.
  source: String,
  /// Size and mtime each file had when its hash was last verified. A file
  /// whose stamp still matches is not hashed again. Manifests written before
  /// stamps existed have none; their files are hashed once and gain one.
  #[serde(default)]
  stamps: HashMap<String, FileStamp>,
}

/// What a verified file looked like on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct FileStamp {
  size: u64,
  mtime_ns: i64,
}

impl FileStamp {
  fn of(meta: &fs::Metadata) -> Self {
    Self {
      size: meta.len(),
      mtime_ns: crate::search::cache::mtime_nanos(meta) as i64,
    }
  }
}

/// Root of the model cache. Honours `AST_BRO_MODEL_DIR` (or legacy
/// `AST_OUTLINE_MODEL_DIR`), then `XDG_CACHE_HOME` (via `dirs::cache_dir`),
/// falling back to `~/.cache/ast-bro/models` on platforms without one.
///
/// Auto-renames `~/.cache/ast-outline/models` -> `~/.cache/ast-bro/models`
/// if the old directory exists and the new one does not.
pub fn cache_root() -> io::Result<PathBuf> {
  if let Ok(custom) = std::env::var("AST_BRO_MODEL_DIR") {
    return Ok(PathBuf::from(custom));
  }
  if let Ok(custom) = std::env::var("AST_OUTLINE_MODEL_DIR") {
    return Ok(PathBuf::from(custom));
  }
  let base = dirs::cache_dir().ok_or_else(|| {
    io::Error::new(
      io::ErrorKind::NotFound,
      "no cache directory found (set AST_BRO_MODEL_DIR)",
    )
  })?;
  let new_dir = base.join("ast-bro");
  let old_dir = base.join("ast-outline");

  // Guard with OnceLock so concurrent callers (e.g. parallel MCP tool calls)
  // don't race on the fs::rename.
  static MIGRATED: OnceLock<()> = OnceLock::new();
  MIGRATED.get_or_init(|| {
    if old_dir.exists() && !new_dir.exists() {
      if let Err(e) = std::fs::rename(&old_dir, &new_dir) {
        eprintln!(
          "warning: could not rename {} -> {}: {e}",
          old_dir.display(),
          new_dir.display()
        );
      } else {
        eprintln!(
          "info: auto-renamed {} -> {}",
          old_dir.display(),
          new_dir.display()
        );
      }
    }
  });

  Ok(new_dir.join("models"))
}

/// Local directory for a single model. Created on demand by `ensure_model`.
pub fn model_dir(info: &ModelInfo) -> io::Result<PathBuf> {
  // Model id like "minishlab/potion-code-16M" -> use only the repo name as
  // the leaf so we don't have to escape the slash.
  let leaf = info.id.split('/').next_back().unwrap_or(&info.id);
  Ok(cache_root()?.join(leaf))
}

/// Make sure all `info.files` exist locally and pass integrity checks.
/// Returns the model directory.
///
/// Will download from HuggingFace (or fall back to hf-mirror.com / a custom
/// mirror) on first call, or whenever cached files fail integrity checks.
pub fn ensure_model(info: &ModelInfo) -> io::Result<PathBuf> {
  let dir = model_dir(info)?;
  fs::create_dir_all(&dir)?;

  if cache_is_valid(&dir, info)? {
    return Ok(dir);
  }

  warn_about_tls_policy();
  let (source, probed) = select_source(info);
  eprintln!(
    "ast-bro: downloading model {} via {} ({} files)",
    info.id,
    source.label(),
    info.files.len()
  );

  let client = build_client(DOWNLOAD_TIMEOUT)?;
  let manifest = download_with_fallback(source, probed, |source| {
    download_all(&client, info, &dir, source)
  })?;
  write_manifest(&dir, &manifest)?;
  Ok(dir)
}

/// Run `download` from `source`, and once more from hf-mirror.com when
/// `source` is HuggingFace picked by the probe and a file failed its pin.
/// The probe only fetched `config.json`'s headers, and a network can let that
/// through yet answer a later file with a page of its own; the mirror may
/// still serve the pinned bytes, and they are checked against the same pins.
/// A source named in `AST_BRO_MODEL_SOURCE` is never switched.
fn download_with_fallback<'a>(
  source: Source<'a>,
  probed: bool,
  mut download: impl FnMut(Source<'a>) -> io::Result<Manifest>,
) -> io::Result<Manifest> {
  match download(source) {
    Err(e) if probed && source == Source::HuggingFace && e.kind() == io::ErrorKind::InvalidData => {
      eprintln!("ast-bro: {e}");
      eprintln!("ast-bro: trying hf-mirror.com instead");
      download(Source::HfMirror)
    }
    result => result,
  }
}

/// Download every file of `info` from `source` into `dir`, each checked
/// against its pin, and return the manifest that records them.
fn download_all(
  client: &reqwest::blocking::Client,
  info: &ModelInfo,
  dir: &Path,
  source: Source<'_>,
) -> io::Result<Manifest> {
  let mut manifest = Manifest {
    source: source.label().to_string(),
    ..Manifest::default()
  };
  for file in &info.files {
    let dest = dir.join(file.name);
    let stamp = download_to(
      client,
      &info.url(source.base_url(), file),
      &dest,
      file.sha256,
    )?;
    manifest
      .sha256
      .insert(file.name.to_string(), file.sha256.to_string());
    manifest.stamps.insert(file.name.to_string(), stamp);
  }
  Ok(manifest)
}

/// The source to download from, and whether the probe picked it rather than
/// `AST_BRO_MODEL_SOURCE`.
fn select_source<'a>(info: &'a ModelInfo) -> (Source<'a>, bool) {
  let forced =
    std::env::var("AST_BRO_MODEL_SOURCE").or_else(|_| std::env::var("AST_OUTLINE_MODEL_SOURCE"));
  if let Ok(forced) = forced {
    match parse_source(&forced) {
      Some(source) => return (source, false),
      // Ignored means as if unset: probed, so the mirror fallback applies.
      None => {
        eprintln!("ast-bro: ignoring AST_BRO_MODEL_SOURCE={forced:?} (use hf, hf-mirror, or a URL)")
      }
    }
  }
  if probe_huggingface(info) {
    (Source::HuggingFace, true)
  } else {
    eprintln!("ast-bro: HuggingFace unreachable, falling back to hf-mirror.com");
    (Source::HfMirror, true)
  }
}

/// The source an `AST_BRO_MODEL_SOURCE` value names, or `None` for a value
/// that names none. A custom base URL loses its trailing slashes, since
/// `ModelInfo::url` puts one after it.
fn parse_source(value: &str) -> Option<Source<'static>> {
  match value {
    "hf" => Some(Source::HuggingFace),
    "hf-mirror" => Some(Source::HfMirror),
    url if url.starts_with("http://") || url.starts_with("https://") => {
      // Leak the string so we can return a 'static borrow. Acceptable —
      // happens once per process at most.
      let base = url.trim_end_matches('/').to_string();
      Some(Source::Custom(Box::leak(base.into_boxed_str())))
    }
    _ => None,
  }
}

fn probe_huggingface(info: &ModelInfo) -> bool {
  let Ok(client) = build_client(PROBE_TIMEOUT) else {
    return false;
  };
  let url = format!(
    "{HF_BASE}/{}/resolve/{}/{PROBE_FILE}",
    info.id, info.revision
  );
  let Ok(resp) = client.head(&url).send() else {
    return false;
  };
  if !resp.status().is_success() {
    return false;
  }
  plausible_probe_size(resp.headers())
}

/// The probe's captive-portal check: a portal answers 200 with a large HTML
/// login page. It reads the `Content-Length` header itself, since reqwest's
/// `content_length()` is the size of the body, which a HEAD response does
/// not have. Only *upper* outliers fail; a missing header is fine, as many
/// CDNs send none on HEAD, and the download is checked against its pinned
/// SHA-256 anyway.
fn plausible_probe_size(headers: &reqwest::header::HeaderMap) -> bool {
  let len = headers
    .get(reqwest::header::CONTENT_LENGTH)
    .and_then(|value| value.to_str().ok())
    .and_then(|value| value.trim().parse::<u64>().ok());
  let hi = (PROBE_REFERENCE_SIZE as f64 * (1.0 + PROBE_SIZE_TOLERANCE)) as u64;
  match len {
    Some(len) if len > hi => {
      eprintln!(
        "ast-bro: HF probe returned implausibly large content-length {len} (expected ≤{hi}); \
         likely a captive portal, falling back"
      );
      false
    }
    _ => true,
  }
}

fn build_client(timeout: Duration) -> io::Result<reqwest::blocking::Client> {
  let mut builder = reqwest::blocking::Client::builder()
    .connect_timeout(timeout)
    .timeout(timeout)
    .user_agent(concat!("ast-bro/", env!("CARGO_PKG_VERSION")));

  // Add an extra CA bundle if the user pointed us at one. Useful behind corp
  // TLS-intercepting proxies whose root is exported as a PEM file.
  let ca_bundle =
    std::env::var("AST_BRO_CA_BUNDLE").or_else(|_| std::env::var("AST_OUTLINE_CA_BUNDLE"));
  if let Ok(bundle) = ca_bundle {
    let pem = fs::read(&bundle)
      .map_err(|e| io::Error::other(format!("AST_BRO_CA_BUNDLE={bundle}: {e}")))?;
    for cert in reqwest::Certificate::from_pem_bundle(&pem)
      .map_err(|e| io::Error::other(format!("invalid CA bundle: {e}")))?
    {
      builder = builder.add_root_certificate(cert);
    }
  }

  // TLS strictness policy:
  // - Default: accept any cert. Lets the downloader work behind corp TLS-MITM
  //   proxies without per-user CA configuration. Integrity is enforced by the
  //   SHA-256 pinned for each file in `ModelInfo`, checked before a download
  //   is moved into the cache, so a MITM channel cannot substitute the model.
  // - `AST_BRO_TLS_STRICT=1` (or legacy `AST_OUTLINE_TLS_STRICT=1`) opts back
  //   into full chain verification.
  let strict = tls_strict();
  if !strict {
    builder = builder.danger_accept_invalid_certs(true);
  }

  builder.build().map_err(io::Error::other)
}

/// Check whether TLS strict mode is enabled via `AST_BRO_TLS_STRICT` or
/// legacy `AST_OUTLINE_TLS_STRICT`.
fn tls_strict() -> bool {
  std::env::var("AST_BRO_TLS_STRICT")
    .or_else(|_| std::env::var("AST_OUTLINE_TLS_STRICT"))
    .ok()
    .filter(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
    .is_some()
}

/// Print the one-time TLS-policy notice. Called from `ensure_model` so it only
/// fires when we're actually about to make outbound requests (not e.g. when
/// loading from cache).
fn warn_about_tls_policy() {
  let strict = tls_strict();
  if !strict {
    eprintln!(
      "ast-bro: TLS certificate verification is DISABLED for model downloads \
             (works through corp MITM proxies). Set AST_BRO_TLS_STRICT=1 to enforce \
             full chain verification. Every file is still checked against the SHA-256 \
             pinned in ast-bro before it is used."
    );
  }
}

/// A temporary path beside `path`: the process id plus a counter. The model
/// cache is shared by every repository, and nothing locks it, so two searches
/// can download the same file at once, and an MCP server can do it from two
/// threads. No two writes of one process draw the same name; two processes
/// can, since containers sharing a cache repeat pids, which is why
/// `TmpFile::create` opens the name exclusively and draws again if it is taken.
fn unique_tmp(path: &Path) -> PathBuf {
  static NEXT: AtomicU64 = AtomicU64::new(0);
  let n = NEXT.fetch_add(1, Ordering::Relaxed);
  // `RandomState` keys come from the OS, so a process that restarts with the
  // same pid (a container) does not draw the names its predecessor left.
  let salt = RandomState::new().hash_one(n);
  let name = path
    .file_name()
    .map(|name| name.to_string_lossy().into_owned())
    .unwrap_or_default();
  path.with_file_name(format!("{name}.{}.{n}.{salt:016x}.tmp", std::process::id()))
}

// A download killed by Ctrl-C or SIGKILL leaves its temporary file behind,
// since no destructor runs to remove it. We do not sweep those files by age:
// a writer suspended mid-download could resume after a long pause, and a
// stale-looking file is not proof its writer is gone. The random part in
// `unique_tmp` means a later download never reuses the name.

/// A temporary file that is deleted when dropped, unless it was renamed into
/// place, so a failed download or write leaves nothing behind.
struct TmpFile {
  path: PathBuf,
  renamed: bool,
}

/// Names `TmpFile::create` draws before giving up. A taken name means another
/// process drew the same pid and counter, so a second collision in a row is
/// already unlikely.
const TMP_NAME_ATTEMPTS: usize = 8;

impl TmpFile {
  /// Create a temporary file beside `path` that this call alone opened, with
  /// the guard that deletes it. A taken name is another writer's file: it is
  /// left alone and a fresh name drawn, so the guard only ever covers a file
  /// this call created.
  fn create(path: &Path) -> io::Result<(Self, fs::File)> {
    Self::create_first(std::iter::repeat_with(|| unique_tmp(path)).take(TMP_NAME_ATTEMPTS))
  }

  /// The first of `candidates` that does not exist yet, created as `create`
  /// does it.
  fn create_first(candidates: impl IntoIterator<Item = PathBuf>) -> io::Result<(Self, fs::File)> {
    let mut taken = None;
    for path in candidates {
      match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
      {
        Ok(file) => {
          let guard = Self {
            path,
            renamed: false,
          };
          return Ok((guard, file));
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => taken = Some(e),
        Err(e) => return Err(e),
      }
    }
    Err(taken.unwrap_or_else(|| {
      io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no temporary file name was free",
      )
    }))
  }

  fn rename_to(mut self, dest: &Path) -> io::Result<()> {
    fs::rename(&self.path, dest)?;
    self.renamed = true;
    Ok(())
  }
}

impl Drop for TmpFile {
  fn drop(&mut self) {
    if !self.renamed {
      let _ = fs::remove_file(&self.path);
    }
  }
}

/// Stream `url` to `dest` via a temporary file of this call's own, and
/// rename it into place only if its SHA-256 is `expected`, so a substituted
/// or truncated file never reaches the cache. The digest covers the bytes
/// this call wrote, and a file no other writer can open is what makes it
/// the digest of the file that gets renamed.
///
/// Returns the stamp of that file, taken before the rename, which keeps size
/// and mtime. Stamping `dest` afterwards would let another process's file,
/// renamed over it in between, inherit the stamp of the verified one.
fn download_to(
  client: &reqwest::blocking::Client,
  url: &str,
  dest: &Path,
  expected: &str,
) -> io::Result<FileStamp> {
  let resp = client.get(url).send().map_err(|e| {
    // reqwest::Error eats the underlying source on Display; chase the
    // chain so TLS/proxy details surface in the error message.
    let mut msg = format!("GET {url}: {e}");
    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&e);
    while let Some(s) = src {
      msg.push_str(&format!(" → {s}"));
      src = s.source();
    }
    io::Error::other(msg)
  })?;
  if !resp.status().is_success() {
    return Err(io::Error::other(format!(
      "GET {url} returned HTTP {}",
      resp.status()
    )));
  }

  let (tmp, mut file) = TmpFile::create(dest)?;
  let mut hasher = Sha256::new();

  let mut reader = resp;
  let mut buf = [0u8; 64 * 1024];
  loop {
    let n = reader.read(&mut buf).map_err(io::Error::other)?;
    if n == 0 {
      break;
    }
    hasher.update(&buf[..n]);
    file.write_all(&buf[..n])?;
  }
  file.sync_all()?;
  drop(file);
  let stamp = FileStamp::of(&fs::metadata(&tmp.path)?);

  let actual = hex_digest(hasher.finalize());
  if actual != expected {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      format!(
        "GET {url}: got sha256 {actual}, but the pinned model has {expected}; \
         the server or a proxy on the way sent different bytes, and nothing was cached"
      ),
    ));
  }
  tmp.rename_to(dest)?;
  Ok(stamp)
}

fn hex_digest<T: AsRef<[u8]>>(bytes: T) -> String {
  let mut s = String::with_capacity(bytes.as_ref().len() * 2);
  for b in bytes.as_ref() {
    use std::fmt::Write;
    let _ = write!(s, "{b:02x}");
  }
  s
}

fn manifest_path(dir: &Path) -> PathBuf {
  dir.join("manifest.json")
}

fn write_manifest(dir: &Path, manifest: &Manifest) -> io::Result<()> {
  let json = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
  // Written beside the manifest and renamed over it: two processes, or two
  // threads of one, can verify the same cache at once, and a torn manifest
  // would send them back to the network.
  let (tmp, mut file) = TmpFile::create(&manifest_path(dir))?;
  file.write_all(&json)?;
  file.sync_all()?;
  drop(file);
  tmp.rename_to(&manifest_path(dir))
}

fn read_manifest(dir: &Path) -> io::Result<Manifest> {
  let bytes = fs::read(manifest_path(dir))?;
  serde_json::from_slice(&bytes).map_err(io::Error::other)
}

#[cfg(test)]
thread_local! {
  /// Files `sha256_file` has hashed on this thread, so tests can tell the
  /// stamp fast path from a re-hash.
  static HASHED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The SHA-256 of the file at `path`, with the stamp of the file it hashed.
/// Both come from one open handle: stamping the path separately could pair
/// this digest with a file renamed over it in between, and a stamp that
/// vouches for bytes nobody hashed.
fn sha256_file(path: &Path) -> io::Result<(String, FileStamp)> {
  #[cfg(test)]
  HASHED.with(|n| n.set(n.get() + 1));
  let mut file = fs::File::open(path)?;
  let stamp = FileStamp::of(&file.metadata()?);
  let mut hasher = Sha256::new();
  let mut buf = [0u8; 64 * 1024];
  loop {
    let n = file.read(&mut buf)?;
    if n == 0 {
      break;
    }
    hasher.update(&buf[..n]);
  }
  Ok((hex_digest(hasher.finalize()), stamp))
}

/// Whether every pinned file is in `dir` with the right bytes.
///
/// A file whose size and mtime match the stamp recorded when it was last
/// verified is trusted without hashing; any other file is hashed and must
/// match its pinned SHA-256. Hashing on every load cost ~120 ms per CLI
/// search, and caught one case a stamp does not: a same-size edit that also
/// puts back the recorded mtime, which takes deliberate effort.
fn cache_is_valid(dir: &Path, info: &ModelInfo) -> io::Result<bool> {
  // Files placed by hand (an offline `AST_BRO_MODEL_DIR`) have no manifest
  // yet; they are hashed against the pinned values like anything else.
  let mut manifest = read_manifest(dir).unwrap_or_else(|_| Manifest {
    source: "local".to_string(),
    ..Manifest::default()
  });
  let mut verified = false;
  for file in &info.files {
    let path = dir.join(file.name);
    let Ok(meta) = fs::metadata(&path) else {
      return Ok(false);
    };
    // A named pipe or device at a model path would block or misbehave when
    // read. The downloader can replace it by rename, so treat it as a miss.
    if !meta.is_file() {
      return Ok(false);
    }
    let pinned = manifest.sha256.get(file.name).map(String::as_str) == Some(file.sha256);
    if pinned && manifest.stamps.get(file.name) == Some(&FileStamp::of(&meta)) {
      // A permission change does not alter size or mtime, so a matching
      // stamp alone does not mean the file can still be opened. Fail over
      // to the recovery path below rather than letting `Embedder::open`
      // fail later.
      match fs::File::open(&path) {
        Ok(_) => continue,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Ok(false),
        Err(e) => return Err(e),
      }
    }
    let (digest, stamp) = match sha256_file(&path) {
      Ok(hashed) => hashed,
      // A file placed by hand but unreadable cannot be trusted, and a
      // writable cache can replace it by download. A read-only cache will
      // fail at the write with a clearer error if it cannot.
      Err(e) if e.kind() == io::ErrorKind::PermissionDenied => return Ok(false),
      Err(e) => return Err(e),
    };
    if digest != file.sha256 {
      eprintln!(
        "ast-bro: cached {} does not match the pinned model, will re-download",
        file.name
      );
      return Ok(false);
    }
    manifest
      .sha256
      .insert(file.name.to_string(), file.sha256.to_string());
    manifest.stamps.insert(file.name.to_string(), stamp);
    verified = true;
  }
  if verified {
    // Best effort: in a read-only cache the files are still valid, they are
    // just hashed again next time.
    let _ = write_manifest(dir, &manifest);
  }
  Ok(true)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn potion_info_pins_three_files() {
    let info = ModelInfo::potion_code_16m();
    assert_eq!(info.id, "minishlab/potion-code-16M");
    let hex = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
    assert!(hex(info.revision, 40), "revision must be a full commit sha");
    let names: Vec<_> = info.files.iter().map(|f| f.name).collect();
    assert_eq!(
      names,
      ["config.json", "tokenizer.json", "model.safetensors"]
    );
    for f in &info.files {
      assert!(hex(f.sha256, 64), "{} needs a full sha256", f.name);
    }
  }

  #[test]
  fn urls_name_the_pinned_revision() {
    let info = ModelInfo::potion_code_16m();
    let url = info.url(HF_MIRROR_BASE, &info.files[2]);
    assert_eq!(
      url,
      format!(
        "https://hf-mirror.com/minishlab/potion-code-16M/resolve/{}/model.safetensors",
        info.revision
      )
    );
  }

  /// A one-file model whose pinned hash is that of `pinned_content`.
  fn fake_model(pinned_content: &[u8]) -> ModelInfo {
    let sha = hex_digest(Sha256::digest(pinned_content));
    ModelInfo {
      id: "fake/model".to_string(),
      revision: "0000000000000000000000000000000000000000",
      files: vec![ModelFile {
        name: "a.txt",
        sha256: Box::leak(sha.into_boxed_str()),
      }],
    }
  }

  fn hashed() -> usize {
    HASHED.with(|n| n.get())
  }

  /// Move `path`'s mtime well away from "now", so the change is visible on
  /// filesystems with coarse timestamps.
  fn shift_mtime(path: &Path, secs: u64) {
    let file = fs::File::options().write(true).open(path).unwrap();
    file
      .set_modified(std::time::SystemTime::now() + Duration::from_secs(secs))
      .unwrap();
  }

  /// One combined test for the two env-var-touching cases. Cargo runs unit
  /// tests in parallel by default, so two tests both setting/unsetting
  /// `AST_BRO_MODEL_DIR` race. Folding them avoids a flake without
  /// pulling in a serial-test crate.
  #[test]
  fn cache_root_and_model_dir_honour_env_override() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().to_path_buf();
    std::env::set_var("AST_BRO_MODEL_DIR", &path);

    let resolved_root = cache_root().unwrap();
    let resolved_model = model_dir(&ModelInfo::potion_code_16m()).unwrap();

    std::env::remove_var("AST_BRO_MODEL_DIR");

    assert_eq!(resolved_root, path);
    assert!(resolved_model.starts_with(&path));
    assert!(resolved_model.ends_with("potion-code-16M"));
  }

  #[test]
  fn cache_invalid_when_manifest_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let info = ModelInfo::potion_code_16m();
    // Empty dir: no manifest → invalid.
    assert!(!cache_is_valid(tmp.path(), &info).unwrap());
  }

  /// The pinned hash decides, not the manifest: a manifest that vouches for
  /// the wrong bytes doesn't make them valid.
  #[test]
  fn cache_invalid_when_file_does_not_match_the_pin() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let info = fake_model(b"pinned bytes");
    fs::write(dir.join("a.txt"), b"other bytes").unwrap();
    let mut manifest = Manifest::default();
    let (actual, _) = sha256_file(&dir.join("a.txt")).unwrap();
    manifest.sha256.insert("a.txt".to_string(), actual);
    let meta = fs::metadata(dir.join("a.txt")).unwrap();
    manifest
      .stamps
      .insert("a.txt".to_string(), FileStamp::of(&meta));
    write_manifest(dir, &manifest).unwrap();
    assert!(!cache_is_valid(dir, &info).unwrap());
  }

  /// A pinned file that cannot be read is a cache miss, not a hard error: a
  /// writable cache can replace it by download. Unix only — Windows file
  /// permissions do not follow the same model.
  #[cfg(unix)]
  #[test]
  fn an_unreadable_cached_file_is_a_miss() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let info = fake_model(b"pinned bytes");
    let path = dir.join("a.txt");
    fs::write(&path, b"pinned bytes").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    // A privileged runner (e.g. CAP_DAC_OVERRIDE) can still read the file,
    // which would make the fixture test nothing.
    if !matches!(
      fs::File::open(&path),
      Err(e) if e.kind() == io::ErrorKind::PermissionDenied
    ) {
      eprintln!("skipping: this runner bypasses file permissions");
      return;
    }

    assert!(!cache_is_valid(dir, &info).unwrap());
  }

  /// A regular file is required: a directory at a model path would otherwise
  /// reach `sha256_file` and error instead of being replaced by download.
  #[test]
  fn a_non_regular_cached_entry_is_a_miss() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let info = fake_model(b"pinned bytes");
    fs::create_dir(dir.join("a.txt")).unwrap();
    assert!(!cache_is_valid(dir, &info).unwrap());
  }

  /// Files placed by hand, with no manifest, are hashed once against the pin
  /// and stamped; after that, loading hashes nothing.
  #[test]
  fn cache_hashes_once_then_trusts_the_stamp() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let info = fake_model(b"hello");
    fs::write(dir.join("a.txt"), b"hello").unwrap();

    let before = hashed();
    assert!(cache_is_valid(dir, &info).unwrap());
    assert_eq!(hashed() - before, 1, "first check hashes");
    let manifest = read_manifest(dir).unwrap();
    assert!(manifest.stamps.contains_key("a.txt"));
    assert_eq!(manifest.source, "local");

    let before = hashed();
    assert!(cache_is_valid(dir, &info).unwrap());
    assert!(cache_is_valid(dir, &info).unwrap());
    assert_eq!(hashed() - before, 0, "a matching stamp skips the hash");
  }

  /// Losing read permission changes neither size nor mtime, so a matching
  /// stamp must not short-circuit into a valid cache. Unix only.
  #[cfg(unix)]
  #[test]
  fn a_stamped_file_that_becomes_unreadable_is_a_miss() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let info = fake_model(b"hello");
    let path = dir.join("a.txt");
    fs::write(&path, b"hello").unwrap();
    assert!(cache_is_valid(dir, &info).unwrap());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if !matches!(
      fs::File::open(&path),
      Err(e) if e.kind() == io::ErrorKind::PermissionDenied
    ) {
      eprintln!("skipping: this runner bypasses file permissions");
      return;
    }
    assert!(!cache_is_valid(dir, &info).unwrap());
  }

  /// A size or mtime change sends the file back through the hash: a touched
  /// file with the same bytes passes and is re-stamped, an edited one fails.
  #[test]
  fn cache_rehashes_a_file_whose_stamp_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let path = dir.join("a.txt");
    let info = fake_model(b"hello");
    fs::write(&path, b"hello").unwrap();
    assert!(cache_is_valid(dir, &info).unwrap());

    shift_mtime(&path, 3600);
    let before = hashed();
    assert!(cache_is_valid(dir, &info).unwrap());
    assert_eq!(hashed() - before, 1, "a moved mtime is re-hashed");
    let before = hashed();
    assert!(cache_is_valid(dir, &info).unwrap());
    assert_eq!(hashed() - before, 0, "and re-stamped");

    fs::write(&path, b"hellO").unwrap();
    shift_mtime(&path, 7200);
    assert!(
      !cache_is_valid(dir, &info).unwrap(),
      "edited bytes fail the pin"
    );
  }

  /// A manifest written before stamps existed still loads; its files are
  /// hashed once and gain a stamp.
  #[test]
  fn manifest_without_stamps_is_upgraded() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let info = fake_model(b"hello");
    fs::write(dir.join("a.txt"), b"hello").unwrap();
    let legacy = format!(
      r#"{{ "sha256": {{ "a.txt": "{}" }}, "source": "hf-mirror" }}"#,
      info.files[0].sha256
    );
    fs::write(manifest_path(dir), legacy).unwrap();

    let before = hashed();
    assert!(cache_is_valid(dir, &info).unwrap());
    assert_eq!(hashed() - before, 1);
    let manifest = read_manifest(dir).unwrap();
    assert!(manifest.stamps.contains_key("a.txt"));
    assert_eq!(manifest.source, "hf-mirror", "the recorded source survives");

    let before = hashed();
    assert!(cache_is_valid(dir, &info).unwrap());
    assert_eq!(hashed() - before, 0);
  }

  #[test]
  fn every_write_gets_its_own_temporary_file() {
    let dest = Path::new("cache").join("model.safetensors");
    let (a, b) = (unique_tmp(&dest), unique_tmp(&dest));
    assert_ne!(a, b);
    assert_eq!(
      a.parent(),
      dest.parent(),
      "renaming must stay on one filesystem"
    );
    let name = a.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
      name.starts_with("model.safetensors.") && name.ends_with(".tmp"),
      "{name}"
    );
    // A random part, so a restarted process with the same pid and a fresh
    // counter does not draw the names its predecessor left behind.
    let salt = name.trim_end_matches(".tmp").rsplit('.').next().unwrap();
    assert!(
      salt.len() == 16 && salt.bytes().all(|b| b.is_ascii_hexdigit()),
      "{name}"
    );
  }

  #[test]
  fn a_temporary_file_is_removed_unless_renamed_into_place() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("a.txt");

    let (abandoned, mut file) = TmpFile::create(&dest).unwrap();
    file.write_all(b"partial").unwrap();
    drop(file);
    let path = abandoned.path.clone();
    drop(abandoned);
    assert!(!path.exists(), "a failed write must leave nothing behind");

    let (finished, mut file) = TmpFile::create(&dest).unwrap();
    file.write_all(b"whole").unwrap();
    drop(file);
    finished.rename_to(&dest).unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"whole");
    assert_eq!(
      fs::read_dir(dir.path()).unwrap().count(),
      1,
      "only the file itself remains"
    );
  }

  /// Two containers sharing a cache can draw the same pid and counter. The
  /// name another writer already created is its file: drawing it must not
  /// arm a guard that deletes it, and the next name is used instead.
  #[test]
  fn a_taken_temporary_name_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (theirs, ours) = (dir.path().join("a.1.0.tmp"), dir.path().join("a.1.1.tmp"));
    fs::write(&theirs, b"another writer's bytes").unwrap();

    let (guard, mut file) = TmpFile::create_first([theirs.clone(), ours.clone()]).unwrap();
    assert_eq!(guard.path, ours);
    file.write_all(b"ours").unwrap();
    drop(file);
    drop(guard);
    assert!(!ours.exists(), "our own file is still cleaned up");
    assert_eq!(fs::read(&theirs).unwrap(), b"another writer's bytes");

    let err = match TmpFile::create_first([theirs.clone()]) {
      Ok(_) => panic!("a taken name must not be opened"),
      Err(e) => e,
    };
    assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&theirs).unwrap(), b"another writer's bytes");
  }

  /// An unrecognized `AST_BRO_MODEL_SOURCE` reads as unset, so the probe
  /// and its mirror fallback still run. A custom base loses trailing
  /// slashes, since `ModelInfo::url` adds one.
  #[test]
  fn model_source_values() {
    assert_eq!(parse_source("hf"), Some(Source::HuggingFace));
    assert_eq!(parse_source("hf-mirror"), Some(Source::HfMirror));
    assert_eq!(
      parse_source("https://your.mirror/"),
      Some(Source::Custom("https://your.mirror"))
    );
    assert_eq!(
      parse_source("http://10.0.0.5:8080"),
      Some(Source::Custom("http://10.0.0.5:8080"))
    );
    assert_eq!(parse_source("typo"), None);
    assert_eq!(parse_source("HF"), None);
    let info = ModelInfo::potion_code_16m();
    let url = info.url(
      parse_source("https://your.mirror//").unwrap().base_url(),
      &info.files[0],
    );
    assert!(
      url.starts_with("https://your.mirror/minishlab/potion-code-16M/resolve/"),
      "{url}"
    );
  }

  /// HuggingFace picked by the probe, then a file that fails its pin: the
  /// mirror gets one try. A named source, another error, or the mirror
  /// itself failing is reported as it is.
  #[test]
  fn only_a_probed_huggingface_falls_back_to_the_mirror_on_a_pin_mismatch() {
    let mismatch = || io::Error::new(io::ErrorKind::InvalidData, "pin mismatch");
    let run =
      |source: Source<'static>, probed: bool, fail: &dyn Fn(Source) -> Option<io::Error>| {
        let mut tried = Vec::new();
        let got = download_with_fallback(source, probed, |s| {
          tried.push(s.label());
          match fail(s) {
            Some(e) => Err(e),
            None => Ok(Manifest {
              source: s.label().to_string(),
              ..Manifest::default()
            }),
          }
        });
        (tried, got.map(|m| m.source).map_err(|e| e.kind()))
      };
    let hf_mismatch = |s: Source| (s == Source::HuggingFace).then(mismatch);

    assert_eq!(
      run(Source::HuggingFace, true, &hf_mismatch),
      (vec!["hf", "hf-mirror"], Ok("hf-mirror".to_string()))
    );
    assert_eq!(
      run(Source::HuggingFace, false, &hf_mismatch),
      (vec!["hf"], Err(io::ErrorKind::InvalidData)),
      "AST_BRO_MODEL_SOURCE=hf stays on HuggingFace"
    );
    assert_eq!(
      run(Source::HuggingFace, true, &|_| Some(io::Error::other(
        "HTTP 503"
      ))),
      (vec!["hf"], Err(io::ErrorKind::Other)),
      "only a pin mismatch switches source; other failures are reported as they are"
    );
    assert_eq!(
      run(Source::HfMirror, true, &|_| Some(mismatch())),
      (vec!["hf-mirror"], Err(io::ErrorKind::InvalidData))
    );
    assert_eq!(
      run(Source::HuggingFace, true, &|_| Some(mismatch())),
      (vec!["hf", "hf-mirror"], Err(io::ErrorKind::InvalidData)),
      "the mirror is checked against the same pins"
    );
  }

  /// Threads of one process share a pid, so a name built from it alone let
  /// one writer truncate or rename away another's temporary file. Unix only:
  /// Windows can refuse a rename onto a target another rename is replacing.
  #[cfg(unix)]
  #[test]
  fn concurrent_manifest_writes_leave_one_whole_manifest() {
    let dir = tempfile::tempdir().unwrap();
    std::thread::scope(|scope| {
      for writer in 0..8 {
        let dir = dir.path();
        scope.spawn(move || {
          for _ in 0..25 {
            let manifest = Manifest {
              source: format!("writer {writer}"),
              ..Manifest::default()
            };
            write_manifest(dir, &manifest).unwrap();
          }
        });
      }
    });
    let manifest = read_manifest(dir.path()).unwrap();
    assert!(
      manifest.source.starts_with("writer "),
      "{}",
      manifest.source
    );
    assert_eq!(
      fs::read_dir(dir.path()).unwrap().count(),
      1,
      "no temporary file is left"
    );
  }

  /// `download_to` stamps its own file before renaming it; that stamp only
  /// matches the file at `dest` if a rename keeps size and mtime.
  #[test]
  fn a_rename_keeps_the_stamp() {
    let dir = tempfile::tempdir().unwrap();
    let (from, to) = (dir.path().join("a.tmp"), dir.path().join("a.txt"));
    fs::write(&from, b"verified bytes").unwrap();
    shift_mtime(&from, 3600);
    let before = FileStamp::of(&fs::metadata(&from).unwrap());
    fs::rename(&from, &to).unwrap();
    assert_eq!(FileStamp::of(&fs::metadata(&to).unwrap()), before);
  }

  #[test]
  fn the_probe_reads_the_content_length_header() {
    use reqwest::header::{HeaderMap, HeaderValue, CONTENT_LENGTH};
    let with = |value: &str| {
      let mut headers = HeaderMap::new();
      headers.insert(CONTENT_LENGTH, HeaderValue::from_str(value).unwrap());
      headers
    };
    assert!(
      !plausible_probe_size(&with("20000")),
      "a login page's size is refused"
    );
    assert!(plausible_probe_size(&with("97")));
    assert!(plausible_probe_size(&HeaderMap::new()), "no header is fine");
    assert!(plausible_probe_size(&with("not a number")));
  }

  #[test]
  fn sha256_matches_known_vector() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("v.txt");
    fs::write(&path, b"abc").unwrap();
    // sha256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
    let (digest, stamp) = sha256_file(&path).unwrap();
    assert_eq!(
      digest,
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(stamp, FileStamp::of(&fs::metadata(&path).unwrap()));
  }

  /// End-to-end download test, ignored by default because it hits the network.
  /// Run with: `cargo test search::download::tests::network -- --ignored --nocapture`
  #[test]
  #[ignore]
  fn network_real_download() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("AST_BRO_MODEL_DIR", tmp.path());
    let info = ModelInfo::potion_code_16m();
    let dir = ensure_model(&info).expect("download failed");
    // Re-validate: the stamps written at download make this hash nothing.
    let before = hashed();
    let dir2 = ensure_model(&info).expect("revalidate failed");
    assert_eq!(hashed() - before, 0, "a fresh download is already stamped");
    assert_eq!(dir, dir2);
    for f in &info.files {
      assert!(dir.join(f.name).exists(), "missing {}", f.name);
    }
    std::env::remove_var("AST_BRO_MODEL_DIR");
  }
}
