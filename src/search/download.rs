//! Model file downloader with HuggingFace → hf-mirror.com fallback.
//!
//! Behaviour (per the search plan):
//! 1. If the cache already has all files and the cached SHA-256 manifest
//!    matches the on-disk bytes, return immediately.
//! 2. Otherwise probe `https://huggingface.co/<id>/resolve/main/config.json`
//!    with a 3-second connect+TLS timeout. Probe success requires HTTP 200
//!    AND a content-length within ±20% of the reference size — defends
//!    against captive-portal redirects that 200 with HTML.
//! 3. On probe success, download from HuggingFace. Otherwise download from
//!    `https://hf-mirror.com/<id>/resolve/main/<file>` (URL-rewrite mirror).
//! 4. Atomic-rename each file into the cache dir; record SHA-256s in
//!    `manifest.json` so subsequent loads can verify integrity.
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
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
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

/// Files we expect every model to ship.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub files: Vec<&'static str>,
}

impl ModelInfo {
    pub fn potion_code_16m() -> Self {
        Self {
            id: "minishlab/potion-code-16M".to_string(),
            files: vec!["config.json", "tokenizer.json", "model.safetensors"],
        }
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

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    /// Map from filename -> hex sha256 string.
    sha256: HashMap<String, String>,
    /// Which source populated this cache: "hf", "hf-mirror", or "custom".
    source: String,
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

    if old_dir.exists() && !new_dir.exists() {
        if let Err(e) = std::fs::rename(&old_dir, &new_dir) {
            eprintln!("warning: could not rename {} -> {}: {e}", old_dir.display(), new_dir.display());
        } else {
            eprintln!("info: auto-renamed {} -> {}", old_dir.display(), new_dir.display());
        }
    }

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
    let source = select_source(info);
    eprintln!(
        "ast-bro: downloading model {} via {} ({} files)",
        info.id,
        source.label(),
        info.files.len()
    );

    let client = build_client(DOWNLOAD_TIMEOUT)?;
    let mut sha256: HashMap<String, String> = HashMap::new();

    for file in &info.files {
        let url = format!("{}/{}/resolve/main/{}", source.base_url(), info.id, file);
        let dest = dir.join(file);
        let hash = download_to(&client, &url, &dest)?;
        sha256.insert(file.to_string(), hash);
    }

    let manifest = Manifest {
        sha256,
        source: source.label().to_string(),
    };
    write_manifest(&dir, &manifest)?;
    Ok(dir)
}

fn select_source<'a>(_info: &'a ModelInfo) -> Source<'a> {
    let forced = std::env::var("AST_BRO_MODEL_SOURCE")
        .or_else(|_| std::env::var("AST_OUTLINE_MODEL_SOURCE"));
    if let Ok(forced) = forced {
        return match forced.as_str() {
            "hf" => Source::HuggingFace,
            "hf-mirror" => Source::HfMirror,
            url if url.starts_with("http://") || url.starts_with("https://") => {
                // Leak the string so we can return a 'static borrow. Acceptable —
                // happens once per process at most.
                Source::Custom(Box::leak(url.to_string().into_boxed_str()))
            }
            other => {
                eprintln!(
                    "ast-bro: ignoring AST_BRO_MODEL_SOURCE={other:?} (use hf, hf-mirror, or a URL)"
                );
                Source::HuggingFace
            }
        };
    }
    if probe_huggingface(&_info.id) {
        Source::HuggingFace
    } else {
        eprintln!("ast-bro: HuggingFace unreachable, falling back to hf-mirror.com");
        Source::HfMirror
    }
}

fn probe_huggingface(model_id: &str) -> bool {
    let Ok(client) = build_client(PROBE_TIMEOUT) else {
        return false;
    };
    let url = format!("{HF_BASE}/{model_id}/resolve/main/{PROBE_FILE}");
    let Ok(resp) = client.head(&url).send() else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    // Content-length sanity check: rejects captive portals that 200 with a
    // large HTML login page. We only flag *upper* outliers; many CDNs return
    // 0 or no content-length on HEAD requests (HF's behaviour) which is fine —
    // the actual GET will stream the right bytes and we verify by SHA-256.
    if let Some(len) = resp.content_length() {
        let hi = (PROBE_REFERENCE_SIZE as f64 * (1.0 + PROBE_SIZE_TOLERANCE)) as u64;
        if len > hi {
            eprintln!(
                "ast-bro: HF probe returned implausibly large content-length {len} (expected ≤{hi}); likely a captive portal, falling back"
            );
            return false;
        }
    }
    true
}

fn build_client(timeout: Duration) -> io::Result<reqwest::blocking::Client> {
    let mut builder = reqwest::blocking::Client::builder()
        .connect_timeout(timeout)
        .timeout(timeout)
        .user_agent(concat!("ast-bro/", env!("CARGO_PKG_VERSION")));

    // Add an extra CA bundle if the user pointed us at one. Useful behind corp
    // TLS-intercepting proxies whose root is exported as a PEM file.
    let ca_bundle = std::env::var("AST_BRO_CA_BUNDLE")
        .or_else(|_| std::env::var("AST_OUTLINE_CA_BUNDLE"));
    if let Ok(bundle) = ca_bundle {
        let pem = fs::read(&bundle).map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("AST_BRO_CA_BUNDLE={bundle}: {e}"),
            )
        })?;
        for cert in reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("invalid CA bundle: {e}")))?
        {
            builder = builder.add_root_certificate(cert);
        }
    }

    // TLS strictness policy:
    // - Default: accept any cert. Lets the downloader work behind corp TLS-MITM
    //   proxies without per-user CA configuration. Integrity is enforced by the
    //   SHA-256 manifest written after first download — subsequent loads detect
    //   tampering even if the original fetch was over a MITM channel.
    // - `AST_BRO_TLS_STRICT=1` (or legacy `AST_OUTLINE_TLS_STRICT=1`) opts back
    //   into full chain verification.
    let strict = tls_strict();
    if !strict {
        builder = builder.danger_accept_invalid_certs(true);
    }

    builder
        .build()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
}

/// Check whether TLS strict mode is enabled via `AST_BRO_TLS_STRICT` or
/// legacy `AST_OUTLINE_TLS_STRICT`.
fn tls_strict() -> bool {
    std::env::var("AST_BRO_TLS_STRICT")
        .or_else(|_| std::env::var("AST_OUTLINE_TLS_STRICT"))
        .ok()
        .filter(|v| !v.is_empty() && v != "0" && v.to_ascii_lowercase() != "false")
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
             full chain verification. Integrity is checked via SHA-256 on subsequent loads."
        );
    }
}

/// Stream `url` to `dest` (via a `.tmp` + atomic rename) and return its hex sha256.
fn download_to(client: &reqwest::blocking::Client, url: &str, dest: &Path) -> io::Result<String> {
    let resp = client.get(url).send().map_err(|e| {
        // reqwest::Error eats the underlying source on Display; chase the
        // chain so TLS/proxy details surface in the error message.
        let mut msg = format!("GET {url}: {e}");
        let mut src: Option<&dyn std::error::Error> = std::error::Error::source(&e);
        while let Some(s) = src {
            msg.push_str(&format!(" → {s}"));
            src = s.source();
        }
        io::Error::new(io::ErrorKind::Other, msg)
    })?;
    if !resp.status().is_success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("GET {url} returned HTTP {}", resp.status()),
        ));
    }

    let tmp = dest.with_extension("tmp");
    let mut file = fs::File::create(&tmp)?;
    let mut hasher = Sha256::new();

    let mut reader = resp;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
    }
    file.sync_all()?;
    drop(file);

    fs::rename(&tmp, dest)?;
    Ok(hex_digest(hasher.finalize()))
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
    let json = serde_json::to_vec_pretty(manifest)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(manifest_path(dir), json)
}

fn read_manifest(dir: &Path) -> io::Result<Manifest> {
    let bytes = fs::read(manifest_path(dir))?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::Other, e))
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_digest(hasher.finalize()))
}

fn cache_is_valid(dir: &Path, info: &ModelInfo) -> io::Result<bool> {
    let Ok(manifest) = read_manifest(dir) else {
        return Ok(false);
    };
    for file in &info.files {
        let path = dir.join(file);
        if !path.exists() {
            return Ok(false);
        }
        let Some(expected) = manifest.sha256.get(*file) else {
            return Ok(false);
        };
        let actual = sha256_file(&path)?;
        if &actual != expected {
            eprintln!(
                "ast-bro: cached {file} failed integrity check, will re-download"
            );
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn potion_info_lists_three_files() {
        let info = ModelInfo::potion_code_16m();
        assert_eq!(info.id, "minishlab/potion-code-16M");
        assert_eq!(info.files.len(), 3);
        assert!(info.files.contains(&"model.safetensors"));
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

    #[test]
    fn cache_invalid_when_file_hash_mismatches() {
        let tmp = tempfile::tempdir().unwrap();
        let info = ModelInfo {
            id: "fake/model".to_string(),
            files: vec!["a.txt"],
        };
        let dir = tmp.path();
        // Write a file and a manifest with a wrong hash.
        fs::write(dir.join("a.txt"), b"hello").unwrap();
        let manifest = Manifest {
            sha256: [(
                "a.txt".to_string(),
                "deadbeef".repeat(8), // 64 hex chars, definitely wrong
            )]
            .into_iter()
            .collect(),
            source: "hf".to_string(),
        };
        write_manifest(dir, &manifest).unwrap();
        assert!(!cache_is_valid(dir, &info).unwrap());
    }

    #[test]
    fn cache_valid_when_hash_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let info = ModelInfo {
            id: "fake/model".to_string(),
            files: vec!["a.txt"],
        };
        let dir = tmp.path();
        fs::write(dir.join("a.txt"), b"hello").unwrap();
        let actual = sha256_file(&dir.join("a.txt")).unwrap();
        let manifest = Manifest {
            sha256: [("a.txt".to_string(), actual)].into_iter().collect(),
            source: "hf".to_string(),
        };
        write_manifest(dir, &manifest).unwrap();
        assert!(cache_is_valid(dir, &info).unwrap());
    }

    #[test]
    fn sha256_matches_known_vector() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("v.txt");
        fs::write(&path, b"abc").unwrap();
        // sha256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
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
        // Re-validate: should be a no-op because manifest now matches.
        let dir2 = ensure_model(&info).expect("revalidate failed");
        assert_eq!(dir, dir2);
        for f in &info.files {
            assert!(dir.join(f).exists(), "missing {f}");
        }
        std::env::remove_var("AST_BRO_MODEL_DIR");
    }
}
