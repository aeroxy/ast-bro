//! Where `implements` looks for the part of a hierarchy its walk cannot
//! see.
//!
//! `implements T <path>` resolves a chain by matching one walked type's
//! `bases` entry against another walked type's name, so a link declared
//! outside `<path>` ends the chain silently: `implements BinaryCodec
//! .../jdbc/codec/` reported 7 of the 52 subtypes that live in that very
//! directory, because the interfaces they inherit through sit one package
//! over.
//!
//! Reporting every base the walk failed to resolve would not do: on a walk
//! of a whole Java source tree that list is 70 names long and is almost
//! entirely JDK types, which no walk can resolve and no wider path would
//! fix. A name is reported only once the project is known to declare it
//! outside the walk *and* the merged declarations prove it inherits the
//! target — the same standard [`crate::path_repair`] holds a path hint to,
//! and the reason this module touches the disk at all.
//!
//! Proving that needs the chain followed to its end, so the probe repeats
//! until the declarations it parsed name nothing new. It terminates because
//! a name is looked up at most once and the project holds finitely many.
//!
//! **How a declaration is located.** By file name, ignoring case and word
//! separators: `StreamingBinaryCodec` is looked for in
//! `StreamingBinaryCodec.java`, `streaming_binary_codec.rs`, and
//! `streaming-binary-codec.ts` alike. That is one directory walk per project
//! and a parse of the handful of files a name matched, where scanning the
//! text of every file for every unresolved name cost 8s of wall clock on the
//! JDK repository.
//!
//! The price is a type whose file is not named after it — an inner type, a
//! Go file holding several — which the probe cannot place. Since an
//! unplaceable name also ends the descent proof of everything above it,
//! silence here is not the same as an answer: names the search failed to
//! place come back in [`HierarchyGap::unlocated`], so a caller reading
//! `hierarchy_truncated: false` can tell "no gap" from "no gap proved".

use crate::core::{self, ParseResult, UnseenBase};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// What the probe learned about the links the walk could not read.
///
/// `unseen` is the proven part: bases the project declares outside the walk
/// whose own declarations show they inherit the target. `unlocated` is the
/// rest of the honest answer — names the search never placed anywhere, each
/// of which is a library type or a declaration this probe cannot find. An
/// empty `unseen` beside a non-empty `unlocated` says no gap was proved, not
/// that there is none.
pub(crate) struct HierarchyGap {
    pub unseen: Vec<UnseenBase>,
    pub unlocated: Vec<String>,
}

impl HierarchyGap {
    /// The answer for a walk that was never probed — `--direct`, where an
    /// unseen ancestor cannot change which types name the target.
    pub(crate) fn none() -> Self {
        Self {
            unseen: Vec::new(),
            unlocated: Vec::new(),
        }
    }
}

/// Look for the part of `target`'s hierarchy the walk could not read.
///
/// Empty on both counts when the walk saw every link it named, and when no
/// project root can be found. Call it only for a transitive answer:
/// `--direct` reports the types that name the target themselves, which an
/// unseen ancestor cannot change.
pub(crate) fn unseen_bases(walked: &[ParseResult], target: &str) -> HierarchyGap {
    let unresolved = core::unresolved_base_names(walked);
    if unresolved.is_empty() {
        return HierarchyGap::none();
    }

    let already: HashSet<PathBuf> = walked
        .iter()
        .map(|r| crate::file_filter::file_identity(&r.path))
        .collect();
    let index = declaration_index(walked);
    if index.is_empty() {
        return HierarchyGap {
            unseen: Vec::new(),
            unlocated: unresolved,
        };
    }

    let mut outside: Vec<ParseResult> = Vec::new();
    let mut asked: Vec<String> = Vec::new();
    let mut probed: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = unresolved.clone();
    while !queue.is_empty() {
        let mut paths: Vec<PathBuf> = Vec::new();
        for name in &queue {
            if !probed.insert(stem_key(name)) {
                continue;
            }
            asked.push(name.clone());
            for p in index.get(&stem_key(name)).into_iter().flatten() {
                // Identity rather than spelling: the walk names a file
                // `src/a.rs` and the index names the same file absolutely,
                // and parsing it twice would make the walk look like the
                // outside. Canonicalizing here rather than while indexing
                // keeps it to the few files a name actually matched.
                if !already.contains(&crate::file_filter::file_identity(p)) {
                    paths.push(p.clone());
                }
            }
        }
        if paths.is_empty() {
            break;
        }
        let round: Vec<ParseResult> = paths
            .par_iter()
            .filter_map(|p| crate::parse_file(p))
            .collect();
        if round.is_empty() {
            break;
        }
        outside.extend(round);
        // What the newly parsed declarations name and nothing yet declares.
        // `probed` keeps the loop from asking twice, which is also what makes
        // it terminate.
        queue = core::unresolved_base_names(walked.iter().chain(outside.iter()))
            .into_iter()
            .filter(|n| !probed.contains(&stem_key(n)))
            .collect();
    }

    // A name still undeclared after the search is one the probe never
    // placed: no file is named after it, or the file that is does not
    // declare it. Either way its ancestry is unread, so a descent proof that
    // ends there proves nothing — which is what `unlocated` reports.
    let declared = core::declared_type_names(walked.iter().chain(outside.iter()));
    let mut unlocated: Vec<String> = asked
        .into_iter()
        .filter(|n| !declared.contains(n))
        .collect();
    unlocated.sort();
    unlocated.dedup();

    HierarchyGap {
        unseen: core::classify_unseen_bases(walked, &outside, target, &unresolved),
        unlocated,
    }
}

/// Index key for a type name or a file stem: lower-cased with `_` and `-`
/// removed, so `StreamingBinaryCodec` finds `StreamingBinaryCodec.java`,
/// `streaming_binary_codec.rs`, and `streaming-binary-codec.ts` alike.
fn stem_key(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// The keys a file is indexed under: its stem, plus the part before the
/// first dot when it has one.
///
/// `Middle.d.ts` has the file stem `Middle.d`, which matches no type name,
/// and a TypeScript declaration file is where an interface a scoped walk
/// inherits often lives. The same second key covers `Middle.test.ts` and
/// friends; the cost of a key that turns out to declare nothing is one parse
/// the name comparison then discards.
fn stem_keys(stem: &str) -> Vec<String> {
    let full = stem_key(stem);
    match stem.split_once('.') {
        Some((head, _)) if !head.is_empty() => {
            let short = stem_key(head);
            if short == full {
                vec![full]
            } else {
                vec![full, short]
            }
        }
        _ => vec![full],
    }
}

/// [`stem_key`] of a file name → the parseable files carrying it, across
/// every project the walk reached into.
///
/// A walk may span repositories (`implements T repo-a/src repo-b/src`), and
/// a link missing from either one is a link missing from the answer, so each
/// distinct root is indexed. Every supported language is indexed too, not
/// only the ones the walk saw: a Java class can inherit a Scala trait, and
/// the missing link is the point of the probe. Files without an extension
/// are skipped — deciding those needs their first line read, and a type
/// whose file is both extensionless and named after it is not a case worth a
/// read of every such file in the project.
///
/// Paths come back spelled the way the walk spelled its own: the note's
/// advice is "add those paths to the walk", and a caller who passed
/// `/tmp/proj/codec` should not be answered in `/private/tmp/…` because the
/// root lookup canonicalized on the way.
fn declaration_index(walked: &[ParseResult]) -> HashMap<String, Vec<PathBuf>> {
    let mut index: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for (root, spelling) in project_roots(walked) {
        let (tx, rx) = std::sync::mpsc::channel();
        let roots = [root.clone()];
        let Some((builder, existing)) = crate::build_filtered_walker(&roots, None) else {
            continue;
        };
        let walker = builder.build_parallel();
        let walk_root = existing[0].clone();
        walker.run(|| {
            let tx = tx.clone();
            let walk_root = walk_root.clone();
            Box::new(move |result| {
                if let Ok(entry) = result {
                    let path = entry.path();
                    if entry.file_type().is_some_and(|ft| ft.is_file())
                        && path.extension().is_some()
                        && crate::main_helpers::can_parse_for_hook(path)
                        && !crate::file_filter::should_skip_path(path, &walk_root)
                    {
                        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                            for key in stem_keys(stem) {
                                let _ = tx.send((key, path.to_path_buf()));
                            }
                        }
                    }
                }
                ignore::WalkState::Continue
            })
        });
        drop(tx);
        for (stem, path) in rx {
            index.entry(stem).or_default().push(spelling.apply(&root, path));
        }
    }
    index
}

/// How a probed path is spelled back to the caller.
///
/// The note's advice is "add those paths to the walk", so its paths have to
/// read like the ones on stdout beside them — the walk's own spelling, not
/// the canonical one [`crate::project_root::find_root_for`] hands back.
enum Spelling {
    /// The caller named the walk absolutely: keep their prefix, symlinks and
    /// all, and hang the rest of the path off it.
    Absolute(PathBuf),
    /// The caller named it relatively: spell every path from the working
    /// directory, however the two are arranged. The directory may sit below
    /// the project root, above it, or beside it in a sibling checkout, and
    /// only the last two need a `..`.
    RelativeTo(PathBuf),
    /// Neither could be established — the walked file would not canonicalize
    /// — so the canonical path is all there is.
    Canonical,
}

impl Spelling {
    fn apply(&self, root: &Path, path: PathBuf) -> PathBuf {
        match self {
            Self::Absolute(prefix) => match path.strip_prefix(root) {
                Ok(rel) => prefix.join(rel),
                Err(_) => path,
            },
            Self::RelativeTo(base) => relative_to(base, &path),
            Self::Canonical => path,
        }
    }
}

/// The projects the walk reached into: canonical root → how paths under it
/// are spelled back.
fn project_roots(walked: &[ParseResult]) -> Vec<(PathBuf, Spelling)> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut roots: Vec<(PathBuf, Spelling)> = Vec::new();
    for r in walked {
        let Some(dir) = r.path.parent() else { continue };
        if !seen.insert(dir.to_path_buf()) {
            continue;
        }
        let Ok(root) = crate::project_root::find_root_for(&r.path) else {
            continue;
        };
        if roots.iter().any(|(known, _)| *known == root) {
            continue;
        }
        let spelling = spelling_for(&r.path, &root);
        roots.push((root, spelling));
    }
    roots
}

/// Pick the spelling from one walked file and the project root it sits in.
fn spelling_for(walked: &Path, root: &Path) -> Spelling {
    if walked.is_relative() {
        return match std::env::current_dir().ok().and_then(|c| c.canonicalize().ok()) {
            Some(cwd) => Spelling::RelativeTo(cwd),
            None => Spelling::Canonical,
        };
    }
    // Drop from the caller's own path as many trailing components as
    // separate the canonical file from the root, and what is left is the
    // caller's spelling of the root itself.
    let Ok(canonical) = walked.canonicalize() else {
        return Spelling::Canonical;
    };
    let Ok(rel) = canonical.strip_prefix(root) else {
        return Spelling::Canonical;
    };
    let mut prefix = walked.to_path_buf();
    for _ in 0..rel.components().count() {
        if !prefix.pop() {
            return Spelling::Canonical;
        }
    }
    Spelling::Absolute(prefix)
}

/// `path` as a route from `base`, with one `..` per level of `base` the two
/// do not share.
///
/// Both are absolute and canonical here, so the shared prefix is the whole
/// of the arrangement: a file under the working directory comes back bare, a
/// sibling checkout as `../other/src/Foo.java`, and a project root above the
/// working directory as `../api/Base.java`.
fn relative_to(base: &Path, path: &Path) -> PathBuf {
    let mut shared = 0;
    let base_parts: Vec<_> = base.components().collect();
    let path_parts: Vec<_> = path.components().collect();
    while shared < base_parts.len()
        && shared < path_parts.len()
        && base_parts[shared] == path_parts[shared]
    {
        shared += 1;
    }
    if shared == 0 {
        return path.to_path_buf(); // different roots: nothing to hang a route off
    }
    let mut out = PathBuf::new();
    for _ in shared..base_parts.len() {
        out.push("..");
    }
    for part in &path_parts[shared..] {
        out.push(part);
    }
    out
}
