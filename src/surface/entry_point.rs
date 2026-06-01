//! Hybrid entry-point discovery: file vs directory, manifest vs convention.
//!
//! Priority order when given a directory:
//!   1. `Cargo.toml` (workspace or single crate)
//!   2. `pyproject.toml` (Python package)
//!   3. `__init__.py` directly in the dir (Python package without manifest)
//!   4. Fallback: walk the dir and let the per-file visibility filter run.
//!
//! When given a file, dispatch by name/extension instead.

use crate::surface::manifest::{self, CargoManifest};
use crate::surface::options::{LangOverride, SurfaceError};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub enum EntryPoint {
    RustCrate {
        root_file: PathBuf,
        crate_name: String,
        #[allow(dead_code)]
        src_dir: PathBuf,
    },
    RustWorkspace {
        members: Vec<EntryPoint>,
    },
    PythonPackage {
        init: PathBuf,
        pkg_name: String,
    },
    /// TypeScript / JavaScript package — resolved entry plus the public
    /// name (the npm package name when `package.json` is present, else
    /// the directory basename).
    TsPackage {
        root_file: PathBuf,
        pkg_name: String,
    },
    /// Scala 3 package — for Scala there's no real "entry file"; the
    /// resolver scans every `.scala` file under `root` and stitches
    /// `export` clauses across them.
    ScalaPackage {
        root: PathBuf,
        #[allow(dead_code)]
        pkg_name: String,
    },
    /// Visibility-filtered walk for languages without re-exports.
    Fallback {
        paths: Vec<PathBuf>,
    },
}

pub fn discover(input: &Path) -> Result<EntryPoint, SurfaceError> {
    if input.is_file() {
        return discover_file(input);
    }
    discover_dir(input)
}

/// Force a particular resolver. Used when the user passes `--lang`.
pub fn discover_as(input: &Path, lang: LangOverride) -> Result<EntryPoint, SurfaceError> {
    match lang {
        LangOverride::Rust => discover_rust(input),
        LangOverride::Python => discover_python(input),
        LangOverride::TypeScript => discover_typescript(input),
        LangOverride::Scala => discover_scala(input),
        LangOverride::Fallback => Ok(EntryPoint::Fallback {
            paths: vec![input.to_path_buf()],
        }),
    }
}

fn discover_file(file: &Path) -> Result<EntryPoint, SurfaceError> {
    let name = file.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let ext = file.extension().and_then(|s| s.to_str()).unwrap_or("");
    if name == "lib.rs" || name == "main.rs" {
        let src_dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
        let crate_name = _crate_name_from_cargo(&src_dir).unwrap_or_else(|| _dir_basename(&src_dir));
        return Ok(EntryPoint::RustCrate {
            root_file: file.to_path_buf(),
            crate_name,
            src_dir,
        });
    }
    if name == "__init__.py" {
        let dir = file.parent().unwrap_or(Path::new("."));
        return Ok(EntryPoint::PythonPackage {
            init: file.to_path_buf(),
            pkg_name: _dir_basename(dir),
        });
    }
    if name == "Cargo.toml" {
        return discover_rust(file.parent().unwrap_or(Path::new(".")));
    }
    if name == "pyproject.toml" {
        return discover_python(file.parent().unwrap_or(Path::new(".")));
    }
    if name == "package.json" {
        return discover_typescript(file.parent().unwrap_or(Path::new(".")));
    }
    if matches!(ext, "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs") {
        let dir = file.parent().unwrap_or(Path::new("."));
        let pkg_name = manifest::parse_package_json(&dir.join("package.json"))
            .and_then(|p| p.name)
            .unwrap_or_else(|| _dir_basename(dir));
        return Ok(EntryPoint::TsPackage {
            root_file: file.to_path_buf(),
            pkg_name,
        });
    }
    if ext == "scala" {
        let dir = file.parent().unwrap_or(Path::new("."));
        return Ok(EntryPoint::ScalaPackage {
            root: dir.to_path_buf(),
            pkg_name: _dir_basename(dir),
        });
    }
    // Last resort: fallback on this single file.
    Ok(EntryPoint::Fallback {
        paths: vec![file.to_path_buf()],
    })
}

fn discover_dir(dir: &Path) -> Result<EntryPoint, SurfaceError> {
    if dir.join("Cargo.toml").is_file() {
        return discover_rust(dir);
    }
    if dir.join("pyproject.toml").is_file() || dir.join("__init__.py").is_file() {
        return discover_python(dir);
    }
    if dir.join("package.json").is_file() {
        return discover_typescript(dir);
    }
    if _has_index_file(dir) {
        return discover_typescript(dir);
    }
    if _has_scala_file(dir) {
        return discover_scala(dir);
    }
    // PHP / Ruby / C++ have no `pub use`-style re-export semantics, so
    // there's no meaningful per-language surface resolver. Recognise their
    // manifests so we route to Fallback explicitly (and skip the deeper
    // probe below) rather than missing the dir entirely.
    if dir.join("composer.json").is_file()
        || dir.join("Gemfile").is_file()
        || dir.join("CMakeLists.txt").is_file()
    {
        return Ok(EntryPoint::Fallback {
            paths: vec![dir.to_path_buf()],
        });
    }
    // Probe one level down for a single-package layout
    // (e.g. a repo where the user is at the top and the crate is in `crates/foo`).
    if let Some(found) = _find_nearest_manifest(dir) {
        return discover(&found);
    }
    Ok(EntryPoint::Fallback {
        paths: vec![dir.to_path_buf()],
    })
}

fn _has_index_file(dir: &Path) -> bool {
    for stem in ["index", "main"] {
        for ext in ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"] {
            if dir.join(format!("{}.{}", stem, ext)).is_file() {
                return true;
            }
        }
    }
    false
}

fn _has_scala_file(dir: &Path) -> bool {
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("scala") {
                return true;
            }
        }
    }
    false
}

fn discover_rust(root: &Path) -> Result<EntryPoint, SurfaceError> {
    let manifest_path = if root.join("Cargo.toml").is_file() {
        root.join("Cargo.toml")
    } else if root.is_file() && root.file_name().and_then(|s| s.to_str()) == Some("Cargo.toml") {
        root.to_path_buf()
    } else {
        return Err(SurfaceError::NoEntryPoint {
            path: root.to_path_buf(),
            hint: "no Cargo.toml here; pass `--lang fallback` or point at lib.rs/main.rs directly".into(),
        });
    };

    let manifest = manifest::parse_cargo_toml(&manifest_path).ok_or_else(|| SurfaceError::Io {
        path: manifest_path.clone(),
        source: std::io::Error::other("cannot read Cargo.toml"),
    })?;

    // Workspace?
    if !manifest.workspace_members.is_empty() {
        let mut members = Vec::new();
        for m in &manifest.workspace_members {
            let member_root = manifest.manifest_dir.join(m);
            if let Ok(ep) = discover_rust(&member_root) {
                members.push(ep);
            }
        }
        if !members.is_empty() {
            return Ok(EntryPoint::RustWorkspace { members });
        }
    }

    let crate_name = manifest
        .package_name
        .clone()
        .unwrap_or_else(|| _dir_basename(&manifest.manifest_dir));

    let root_file = _resolve_rust_root(&manifest);
    if let Some(rf) = root_file {
        let src_dir = rf.parent().unwrap_or(&manifest.manifest_dir).to_path_buf();
        return Ok(EntryPoint::RustCrate {
            root_file: rf,
            crate_name,
            src_dir,
        });
    }
    Err(SurfaceError::NoEntryPoint {
        path: manifest.manifest_dir.clone(),
        hint: "Cargo.toml found but no lib.rs/main.rs and no [lib].path/[[bin]].path entry".into(),
    })
}

fn discover_python(root: &Path) -> Result<EntryPoint, SurfaceError> {
    // Direct __init__.py in the dir wins.
    let direct = root.join("__init__.py");
    if direct.is_file() {
        let pkg_name = manifest::parse_pyproject_toml(&root.join("pyproject.toml"))
            .and_then(|p| p.project_name)
            .unwrap_or_else(|| _dir_basename(root));
        return Ok(EntryPoint::PythonPackage {
            init: direct,
            pkg_name,
        });
    }
    // Otherwise look for a child dir that is a package (single-package layout).
    if let Ok(read) = std::fs::read_dir(root) {
        for entry in read.flatten() {
            let p = entry.path();
            if p.is_dir() && p.join("__init__.py").is_file() {
                let pkg_name = manifest::parse_pyproject_toml(&root.join("pyproject.toml"))
                    .and_then(|x| x.project_name)
                    .unwrap_or_else(|| _dir_basename(&p));
                return Ok(EntryPoint::PythonPackage {
                    init: p.join("__init__.py"),
                    pkg_name,
                });
            }
        }
    }
    Err(SurfaceError::NoEntryPoint {
        path: root.to_path_buf(),
        hint: "no __init__.py here or in any immediate subdirectory".into(),
    })
}

fn discover_typescript(root: &Path) -> Result<EntryPoint, SurfaceError> {
    let pkg_path = root.join("package.json");
    if pkg_path.is_file() {
        if let Some(pkg) = manifest::parse_package_json(&pkg_path) {
            if let Some(entry_file) = manifest::resolve_package_entry(&pkg) {
                let pkg_name = pkg
                    .name
                    .clone()
                    .unwrap_or_else(|| _dir_basename(&pkg.manifest_dir));
                return Ok(EntryPoint::TsPackage {
                    root_file: entry_file,
                    pkg_name,
                });
            }
        }
    }
    // No (or unresolvable) package.json — try index files at the root.
    for stem in ["index", "main", "src/index", "src/main"] {
        for ext in ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"] {
            let cand = root.join(format!("{}.{}", stem, ext));
            if cand.is_file() {
                return Ok(EntryPoint::TsPackage {
                    root_file: cand,
                    pkg_name: _dir_basename(root),
                });
            }
        }
    }
    Err(SurfaceError::NoEntryPoint {
        path: root.to_path_buf(),
        hint: "no package.json with resolvable `exports`/`main`/`module`/`types`, and no index.* in the dir".into(),
    })
}

fn discover_scala(root: &Path) -> Result<EntryPoint, SurfaceError> {
    if !root.is_dir() {
        let dir = root.parent().unwrap_or(Path::new("."));
        return Ok(EntryPoint::ScalaPackage {
            root: dir.to_path_buf(),
            pkg_name: _dir_basename(dir),
        });
    }
    Ok(EntryPoint::ScalaPackage {
        root: root.to_path_buf(),
        pkg_name: _dir_basename(root),
    })
}

fn _resolve_rust_root(m: &CargoManifest) -> Option<PathBuf> {
    if let Some(p) = &m.lib_path {
        let abs = m.manifest_dir.join(p);
        if abs.is_file() {
            return Some(abs);
        }
    }
    let default_lib = m.manifest_dir.join("src/lib.rs");
    if default_lib.is_file() {
        return Some(default_lib);
    }
    let default_main = m.manifest_dir.join("src/main.rs");
    if default_main.is_file() {
        return Some(default_main);
    }
    for b in &m.bins {
        if let Some(p) = &b.path {
            let abs = m.manifest_dir.join(p);
            if abs.is_file() {
                return Some(abs);
            }
        }
    }
    None
}

fn _find_nearest_manifest(dir: &Path) -> Option<PathBuf> {
    let read = std::fs::read_dir(dir).ok()?;
    for entry in read.flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        if p.join("Cargo.toml").is_file() || p.join("pyproject.toml").is_file()
            || p.join("__init__.py").is_file()
        {
            return Some(p);
        }
    }
    None
}

fn _crate_name_from_cargo(src_dir: &Path) -> Option<String> {
    // src_dir is e.g. .../mycrate/src ; manifest is one up.
    let parent = src_dir.parent()?;
    let manifest = parent.join("Cargo.toml");
    if !manifest.is_file() {
        return None;
    }
    manifest::parse_cargo_toml(&manifest).and_then(|m| m.package_name)
}

fn _dir_basename(p: &Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string()
}
