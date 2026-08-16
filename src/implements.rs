//! `implements` — the transitive subtype closure of a type.
//!
//! The hard part is not the walk but the edges: a supertype reference is
//! written as a name, and a name can be declared in more than one package.
//! Matching those by simple name merges the declarations, and every subtype
//! of one is then reported as a subtype of the other. `_resolve_base` pins a
//! reference to a single declaration where the source says which one is
//! meant, and reports the reference instead of guessing where it does not.
//!
//! Shared by the CLI and the MCP tool so the two cannot drift.

use crate::core::{Declaration, DeclarationKind, ParseResult, JSON_SCHEMA_IMPLEMENTS};
use serde::Serialize;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Whether any *type* declaration in `results` matches `target`. Only
/// type-shaped kinds count as proof: a function named `helper` must not
/// validate `implements helper`. Enums and C# delegates do count, so a
/// 0-match answer on one of those is legitimate rather than a rejection.
///
/// The fallback half of [`target_exists`], which is the gate callers want.
fn _declares_type(results: &[ParseResult], target: &str) -> bool {
    results.iter().any(|r| {
        crate::core::find_symbols(r, target).iter().any(|m| {
            matches!(
                m.kind.as_str(),
                "class" | "struct" | "interface" | "record" | "enum" | "delegate"
            )
        })
    })
}

#[derive(Clone, Serialize)]
pub struct ImplMatch {
    pub path: String,
    pub start_line: usize,
    pub kind: String,
    pub name: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub via: Vec<String>,
}

/// A supertype reference that named a type declared in more than one
/// package, where at least one of the colliding declarations is inside the
/// answer. The edge is dropped rather than guessed, and a dropped edge is
/// reported for the same reason a `--limit` or `--depth` cutoff is.
#[derive(Clone, Serialize)]
pub struct AmbiguousBase {
    /// File holding the subtype whose supertype reference could not be pinned.
    pub path: String,
    pub start_line: usize,
    /// The subtype that would have joined the answer had the edge resolved.
    pub name: String,
    /// The supertype reference as written in the source.
    pub base: String,
    /// Every declaration the reference could have meant, sorted. Each
    /// carries where it is declared, because a qualified name does not
    /// always tell them apart: in a language with no namespace syntax both
    /// candidates are just `Base`.
    pub candidates: Vec<BaseCandidate>,
}

/// One declaration a supertype reference could have meant.
#[derive(Clone, Serialize)]
pub struct BaseCandidate {
    pub qname: String,
    pub path: String,
    pub start_line: usize,
}

fn _candidate(t: &TypeEntry<'_>) -> BaseCandidate {
    BaseCandidate {
        qname: t.qname.clone(),
        path: t.path.to_string_lossy().to_string(),
        start_line: t.decl.start_line,
    }
}

impl std::fmt::Display for BaseCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}:{})", self.qname, self.path, self.start_line)
    }
}

/// The `implements` answer: the closure plus the edges left out of it.
pub struct ImplementsReport {
    pub matches: Vec<ImplMatch>,
    /// Dropped edges, in source order. Empty on a repository where no
    /// supertype name collides — the ordinary case.
    pub ambiguous: Vec<AmbiguousBase>,
    /// Declarations the target name itself matched, when more than one; the
    /// answer then covers all of them. Empty otherwise.
    pub target_candidates: Vec<BaseCandidate>,
    /// Whether the target resolved to at least one declaration in the walk.
    /// Separate from `matches` being empty, which is a legitimate answer.
    pub target_found: bool,
    /// Distinct qualified spellings a bare out-of-walk target covered, when
    /// the walk holds more than one. Empty otherwise.
    pub external_spellings: Vec<String>,
}

/// Every type under `results` that reaches `type_name` through its
/// supertype references, plus the qualifications the answer carries.
///
/// `type_name` may be bare (`TextCodec`) or qualified
/// (`org.postgresql.jdbc.codec.TextCodec`); a bare name that several
/// packages declare answers for all of them and reports that it did. With
/// `transitive` off the walk stops at direct subtypes.
pub fn find_implementations(
    results: &[ParseResult],
    type_name: &str,
    transitive: bool,
) -> ImplementsReport {
    let types = _collect_types(results);
    let index = _name_index(&types);

    // The target is a type reference like any other, so it goes through the
    // same narrowing: `implements TextCodec` names both `TextCodec`s and
    // answers for both, `implements jdbc.codec.TextCodec` names one.
    let target_segments = _type_ref_segments(type_name);
    let target_simple = _normalize_type_name(type_name);
    let declared = index.get(&target_simple);
    let mut roots: Vec<usize> = declared
        .map(|v| _resolve_target(&types, v, &target_segments))
        .unwrap_or_default();
    roots.sort_unstable();

    // Resolve every supertype reference once, up front: which declaration it
    // means does not depend on which parent we happen to be expanding.
    let mut edges: Vec<Vec<usize>> = vec![Vec::new(); types.len()];
    let mut unpinned: Vec<(usize, &str, Vec<usize>)> = Vec::new();
    let imports = ImportIndex::new(results);
    for (i, t) in types.iter().enumerate() {
        for b in &t.decl.bases {
            match _resolve_base(&imports, &types, &index, i, b) {
                BaseRef::One(target) => edges[target].push(i),
                BaseRef::Several(cands) => unpinned.push((i, b.as_str(), cands)),
                BaseRef::None => {}
            }
        }
    }

    // Direct subtypes. When the target resolved, they are the references
    // that resolved to it. When it did not, the supertype is outside the
    // walk — `implements Runnable` over an application tree names the JDK's
    // — and the references that spell that name are the answer.
    //
    // How much a reference has to spell depends on what the walk can say.
    // Where no declaration carries the name, a bare `implements Runnable`
    // can mean nothing else, so it counts. Where the walk *does* declare
    // the name and the qualifier rejected every declaration, a bare
    // reference most likely means one of those declarations, and only a
    // reference whose own qualifier agrees is evidence for the target: that
    // is what keeps `implements no.such.TextCodec` a rejection while
    // `implements java.lang.Runnable` still answers next to an unrelated
    // `mylib.Runnable`.
    let direct: Vec<usize> = if roots.is_empty() {
        let need_qualifier = declared.is_some();
        types
            .iter()
            .enumerate()
            .filter(|(i, t)| {
                t.decl.bases.iter().any(|b| {
                    _names_external_target(b, &target_simple, &target_segments, need_qualifier)
                        || _import_names_external_target(
                            &imports,
                            &types,
                            *i,
                            b,
                            &target_simple,
                            &target_segments,
                        )
                })
            })
            .map(|(i, _)| i)
            .collect()
    } else {
        let mut v: Vec<usize> = roots
            .iter()
            .flat_map(|&r| edges[r].iter().copied())
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    // Which roots each direct subtype came from, so the walk can keep a type
    // out of its own answer. Every one of them, not the first: a subtype two
    // roots both name is two branches of the walk, and past a cycle they do
    // not see the same nodes. A target the walk does not declare has no root
    // to come from, and `usize::MAX` is an index no type has.
    let origins_of = |d: usize| -> Vec<usize> {
        let out: Vec<usize> = roots
            .iter()
            .copied()
            .filter(|&r| edges[r].contains(&d))
            .collect();
        if out.is_empty() {
            vec![usize::MAX]
        } else {
            out
        }
    };

    let external_spellings = if roots.is_empty() {
        _external_spellings(&imports, &types, &target_simple, &direct)
    } else {
        Vec::new()
    };

    let mut closure: Vec<(usize, Vec<String>)> = Vec::new();
    // Nothing is visited before the walk starts, roots included: a bare
    // target that several declarations answer for can hold one that inherits
    // from another — `b.Base extends a.Base` — and that one is an answer to
    // the query as much as any other subtype is. Seeding the visited set with
    // the roots deleted it, while the note beside the answer went on claiming
    // to cover every declaration the name matched.
    //
    // What a type must not be is its own subtype, and the seeded set was what
    // enforced that. It is not a self-edge — `_candidates_for` already refuses
    // one — but a cycle: `class A extends B` beside `class B extends A` walks
    // out of `A` and back into it, and `implements A` reported `class A [via
    // B]`. Each branch of the walk therefore carries the root it started
    // from, which is the only thing it may not arrive at.
    //
    // Which makes the visited set per branch rather than shared. A type two
    // roots both reach is one answer, but it is two branches, and stopping
    // the second at the shared node would leave whatever lies past it
    // reachable only through the origin the first branch may not return to.
    // The closure dedupes on its own, so a node still appears once, under the
    // first chain that found it.
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut in_closure: HashSet<usize> = HashSet::new();
    // A direct subtype carries no chain; every hop past it appends the type
    // it came through, and the renderer shows the last one.
    let mut frontier: Vec<(usize, usize, Vec<String>)> = Vec::new();
    for d in direct {
        for origin in origins_of(d) {
            if d == origin || !seen.insert((origin, d)) {
                continue;
            }
            if in_closure.insert(d) {
                closure.push((d, Vec::new()));
            }
            frontier.push((origin, d, Vec::new()));
        }
    }
    while transitive && !frontier.is_empty() {
        let mut next = Vec::new();
        for (origin, parent, via) in frontier {
            for &child in &edges[parent] {
                if child == origin || !seen.insert((origin, child)) {
                    continue;
                }
                let mut chain = via.clone();
                chain.push(types[parent].decl.name.clone());
                if in_closure.insert(child) {
                    closure.push((child, chain.clone()));
                }
                next.push((origin, child, chain));
            }
        }
        frontier = next;
    }

    let matches = closure
        .iter()
        .map(|(i, via)| _impl_match(types[*i].path, types[*i].decl, via.clone()))
        .collect();

    // An unpinned reference is only worth reporting when pinning it the
    // other way would have grown the answer: one of its candidates is in the
    // answer and the subtype is not there already. Everything else is a
    // collision somewhere else in the repository, which this query is not
    // about.
    //
    // The answer is the closure plus the roots, which the walk no longer
    // visits on its own. What a candidate has to be, though, depends on how
    // deep this answer goes: with `--direct` a reference pinned to anything
    // but a root would have made the referring type a *transitive* subtype,
    // which this answer does not report at all, so reporting it asks the
    // reader to disambiguate something that cannot change what they see.
    let in_answer: HashSet<usize> = in_closure
        .iter()
        .copied()
        .chain(roots.iter().copied())
        .collect();
    let could_grow: HashSet<usize> = if transitive {
        in_answer.clone()
    } else {
        roots.iter().copied().collect()
    };
    let mut ambiguous: Vec<AmbiguousBase> = unpinned
        .into_iter()
        .filter(|(subtype, _, cands)| {
            !in_answer.contains(subtype) && cands.iter().any(|c| could_grow.contains(c))
        })
        .map(|(subtype, base, cands)| {
            let t = &types[subtype];
            let mut candidates: Vec<BaseCandidate> =
                cands.iter().map(|&c| _candidate(&types[c])).collect();
            candidates.sort_by(|a, b| (&a.path, a.start_line).cmp(&(&b.path, b.start_line)));
            AmbiguousBase {
                path: t.path.to_string_lossy().to_string(),
                start_line: t.decl.start_line,
                name: t.decl.name.clone(),
                base: base.to_string(),
                candidates,
            }
        })
        .collect();
    ambiguous.sort_by(|a, b| (&a.path, a.start_line).cmp(&(&b.path, b.start_line)));

    let target_candidates = if roots.len() > 1 {
        roots.iter().map(|&r| _candidate(&types[r])).collect()
    } else {
        Vec::new()
    };

    ImplementsReport {
        matches,
        ambiguous,
        target_candidates,
        target_found: !roots.is_empty(),
        external_spellings,
    }
}

/// Did the query name a type this walk can see? A 0-match answer is only
/// honest for a type that exists (issue #36), and the two answers have to
/// be told apart by the caller's exit code.
///
/// For a qualified target the resolution `find_implementations` already did
/// is the authoritative answer — `implements a.b.Missing` must not pass the
/// gate just because some other package declares `Missing`. An unqualified
/// one falls back to the wider declaration scan, which also counts enums
/// and C# delegates: kinds that can be an `implements` target without ever
/// appearing in a closure.
pub fn target_exists(results: &[ParseResult], report: &ImplementsReport, target: &str) -> bool {
    report.target_found || (_type_ref_segments(target).len() < 2 && _declares_type(results, target))
}

/// The qualifications an `implements` answer carries, one line each and
/// already prefixed with `# note:`. Written once so the CLI (which sends
/// them to stderr) and MCP (which prepends them to the body, having one
/// channel) cannot word the same caveat two ways.
pub fn implements_notes(report: &ImplementsReport, target: &str) -> Vec<String> {
    let mut out = Vec::new();
    if !report.external_spellings.is_empty() {
        out.push(format!(
            "# note: '{}' is not declared in the given path(s) and is written {} \
             different ways there ({}); the answer covers all of them. Qualify the target \
             to pick one.",
            target,
            report.external_spellings.len(),
            report.external_spellings.join(", ")
        ));
    }
    if !report.target_candidates.is_empty() {
        out.push(format!(
            "# note: '{}' is declared in {} places ({}); the answer covers all of them. \
             Qualify the target to pick one.",
            target,
            report.target_candidates.len(),
            report
                .target_candidates
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !report.ambiguous.is_empty() {
        let shown = report.ambiguous.len().min(crate::defaults::AMBIGUOUS_SHOWN);
        out.push(format!(
            "# note: {} supertype reference(s) left out of the answer — the name is declared \
             in more than one package and the referring file does not pick one:",
            report.ambiguous.len()
        ));
        for a in &report.ambiguous[..shown] {
            out.push(format!(
                "#   {}:{} {} -> '{}' could be {}",
                a.path,
                a.start_line,
                a.name,
                a.base,
                a.candidates
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(" or ")
            ));
        }
        if report.ambiguous.len() > shown {
            out.push(format!(
                "#   … and {} more (the full list is in --json)",
                report.ambiguous.len() - shown
            ));
        }
    }
    out
}

/// One declared type, with the two names that decide what a bare reference
/// to it can mean: the package it sits in and its fully qualified name.
///
/// Every type-shaped kind is collected, enums and C# delegates included.
/// They are legitimate `implements` targets — an answer of "this enum has
/// no subtypes" is a real answer, not a rejected query — and in Java, C#
/// and Kotlin an enum implements interfaces, so it belongs in a closure as
/// a subtype too.
struct TypeEntry<'a> {
    /// Index of the owning `ParseResult`, so a reference can consult the
    /// imports of the file it was written in.
    file: usize,
    /// The adapter's language name. A supertype reference never crosses a
    /// language, so this bounds the candidates before any rule runs.
    lang: &'static str,
    path: &'a Path,
    decl: &'a Declaration,
    /// Enclosing `Namespace` chain, dot-joined. Empty for languages whose
    /// module is a file rather than a declaration (Python, TypeScript) —
    /// the same-package rule then simply does not apply, which is correct:
    /// two Python modules in one directory do not see each other's names.
    package: String,
    /// `package`, the enclosing type names, and this type's own name.
    qname: String,
}

fn _collect_types(results: &[ParseResult]) -> Vec<TypeEntry<'_>> {
    let mut out = Vec::new();
    for (file, r) in results.iter().enumerate() {
        _collect_types_in(&r.declarations, &r.path, file, r.language, "", "", &mut out);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn _collect_types_in<'a>(
    decls: &'a [Declaration],
    path: &'a Path,
    file: usize,
    lang: &'static str,
    package: &str,
    enclosing: &str,
    out: &mut Vec<TypeEntry<'a>>,
) {
    use DeclarationKind::*;
    for d in decls {
        match d.kind {
            Namespace => {
                let package = _join_dot(package, &d.name);
                _collect_types_in(&d.children, path, file, lang, &package, "", out);
            }
            Class | Struct | Interface | Record | Enum | Delegate => {
                out.push(TypeEntry {
                    file,
                    lang,
                    path,
                    decl: d,
                    package: package.to_string(),
                    qname: _join_dot(&_join_dot(package, enclosing), &d.name),
                });
                let enclosing = _join_dot(enclosing, &d.name);
                _collect_types_in(&d.children, path, file, lang, package, &enclosing, out);
            }
            _ => _collect_types_in(&d.children, path, file, lang, package, enclosing, out),
        }
    }
}

fn _name_index<'a>(types: &[TypeEntry<'a>]) -> HashMap<String, Vec<usize>> {
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, t) in types.iter().enumerate() {
        index
            .entry(_normalize_type_name(&t.decl.name))
            .or_default()
            .push(i);
    }
    index
}

/// What an import makes visible.
#[derive(Clone, PartialEq)]
enum Bind {
    /// Exactly this simple name: `import a.b.TextCodec`, `use a::b::Client`.
    Name(String),
    /// Some export of a module the specifier names, without saying which —
    /// `import { Base } from './base'` puts the bound names in the clause
    /// rather than the specifier, and the extractors keep the specifier.
    Module,
    /// A wildcard: `import a.b.*`, `from a.b import *`, `export * from …`.
    /// Ranks below the referring file's own package, in the resolver as in
    /// the languages.
    OnDemand,
}

/// One import statement, reduced to what a supertype reference needs from
/// it: the name it binds and the path it names.
#[derive(Clone)]
struct FileImport {
    binds: Bind,
    /// Segments of the imported path: `["org", "postgresql", "api",
    /// "codec", "TextCodec"]`, or `["helpers"]` for `from './helpers'`.
    segments: Vec<String>,
    /// 1-indexed line the import was written on, which is what places it
    /// inside or outside a namespace body.
    line: u32,
    /// Where a *relative* specifier (`./base`, `../a/b`) lands, extension
    /// stripped. `None` for an absolute one. Resolving these is what makes
    /// a TypeScript or Python sibling import decide between two files of
    /// the same name — the specifier's own segments cannot.
    resolved: Option<PathBuf>,
}

/// The imports of each walked file, extracted on demand and cached for the
/// life of one query, so a file is reparsed at most once however many
/// references consult it.
///
/// What it is not gated on is the name collision that motivated the index.
/// Three rules read imports and each pays on its own terms. The rename rules
/// ask [`may_rename`] first, and since a rename outranks the declaration
/// index they run before it — so in a language that can rename, every
/// reference in a file whose text hints at one pays. The qualifier expander
/// and the two import narrowings ask nothing: they extract whenever they
/// run, which is a reference spelling more than one segment, or a simple
/// name two declarations carry. A Java tree therefore does extract, on
/// exactly the queries this resolver exists for, however little Java can
/// rename.
///
/// The cost that follows is about a `map` plus a `deps` extraction over the
/// same tree — measured at 1.5s against 0.3s over 550 Python files of ~7 KB.
/// A probe keyed on the name the reference spells would cut it and is the
/// wrong trade: a false negative there deletes an edge and reports nothing.
///
/// Extraction itself is the one the dep graph uses, rather than a second one
/// that could disagree with it.
///
/// [`may_rename`]: ImportIndex::may_rename
struct ImportIndex<'a> {
    results: &'a [ParseResult],
    cache: RefCell<HashMap<usize, std::rc::Rc<Vec<FileImport>>>>,
    renames: RefCell<HashMap<usize, bool>>,
}

impl<'a> ImportIndex<'a> {
    fn new(results: &'a [ParseResult]) -> Self {
        Self {
            results,
            cache: RefCell::new(HashMap::new()),
            renames: RefCell::new(HashMap::new()),
        }
    }

    /// Could this file rename an import at all?
    ///
    /// Extraction reparses the file, so a file that cannot possibly rename
    /// anything is worth skipping — but the two failure directions are not
    /// symmetric. A false positive costs one reparse; a false negative
    /// deletes an edge and reports nothing. So the answer errs high, and
    /// the work is split accordingly: the language check is a fact and does
    /// the rejecting (Java has no import rename in any form, so no Java
    /// file ever takes this path), while the text probes are whole-file and
    /// loose and reject little else. `as` and `=>` are ordinary syntax in
    /// C#, Rust and TypeScript, so those languages mostly pay the
    /// extraction — measured at 0.16s against a 0.07s `map` baseline over
    /// 2000 C# files, which buys no silent losses.
    ///
    /// Anchoring the probes to lines that *start* an import was the tighter
    /// version and the wrong trade: `pub use a::B as C` and a rustfmt-wrapped
    /// `use a::{\n  B as C,\n}` both begin their rename on a continuation
    /// line, and both silently lost their edge.
    fn may_rename(&self, file: usize) -> bool {
        if let Some(hit) = self.renames.borrow().get(&file) {
            return *hit;
        }
        let lang = crate::deps::resolver::build::Lang::from_path(&self.results[file].path);
        let src = String::from_utf8_lossy(&self.results[file].source);
        let hit = lang.is_some_and(_can_rename_an_import)
            && (src.contains(" as ")
                || src.contains("=>")
                || (src.contains("using") && src.contains('=')));
        self.renames.borrow_mut().insert(file, hit);
        hit
    }

    /// Can an import written at `import_line` be seen from `decl_line`?
    ///
    /// A C# `using X = …` inside `namespace A` is invisible in a sibling
    /// `namespace B`, and a Rust `use … as …` inside a `mod` block is
    /// invisible outside it. The extractor reports one flat list per file
    /// with no scope attached, but the scope is recoverable: an import
    /// inside a namespace's line range serves only declarations in that
    /// range, and one outside every namespace serves the whole file.
    fn visible_at(&self, file: usize, import_line: u32, decl_line: usize) -> bool {
        match _innermost_namespace(&self.results[file].declarations, import_line as usize) {
            Some((start, end)) => decl_line >= start && decl_line <= end,
            None => true,
        }
    }

    /// How wide the scope an import written at `import_line` serves is, in
    /// lines. The whole file is the widest there is.
    ///
    /// This orders two visible bindings of one name, which is a question
    /// visibility alone cannot answer: a `using Base = B.Root` inside
    /// `namespace N` and a file-scope `using Base = A.Root` are both visible
    /// to a class in `N`, and the language says the narrower one is the one
    /// in force.
    fn scope_span(&self, file: usize, import_line: u32) -> usize {
        match _innermost_namespace(&self.results[file].declarations, import_line as usize) {
            Some((start, end)) => end.saturating_sub(start),
            None => usize::MAX,
        }
    }

    /// The imports of `file` that a declaration at `line` can see, innermost
    /// scope first.
    ///
    /// This is the only way in. Scoping was a check applied at the call
    /// sites that remembered it, and three of five did not — an alias from
    /// one Rust `mod` expanded a qualifier in its sibling, and the
    /// out-of-walk path took a rename that was never in scope. Putting the
    /// filter inside the accessor leaves no call site able to forget.
    ///
    /// The order is part of the answer, for the same reason: where two
    /// visible imports bind one name, the inner one shadows the outer, so a
    /// caller that takes the first match takes the binding in force. Sorting
    /// is stable, so imports sharing a scope keep source order.
    fn visible_from(&self, file: usize, line: usize) -> Vec<FileImport> {
        let mut out: Vec<FileImport> = self
            .all(file)
            .iter()
            .filter(|i| self.visible_at(file, i.line, line))
            .cloned()
            .collect();
        out.sort_by_key(|i| self.scope_span(file, i.line));
        out
    }

    fn all(&self, file: usize) -> std::rc::Rc<Vec<FileImport>> {
        if let Some(hit) = self.cache.borrow().get(&file) {
            return hit.clone();
        }
        let path = &self.results[file].path;
        let lang = crate::deps::resolver::build::Lang::from_path(path);
        let raw = match lang {
            Some(lang) => crate::deps::extract::extract(path, lang),
            None => Vec::new(),
        };
        let parsed = std::rc::Rc::new(
            raw.iter()
                .map(|r| FileImport::from_raw(r, path, lang))
                .collect::<Vec<_>>(),
        );
        self.cache.borrow_mut().insert(file, parsed.clone());
        parsed
    }
}

impl FileImport {
    fn from_raw(
        raw: &crate::deps::extract::RawImport,
        from: &Path,
        lang: Option<crate::deps::resolver::build::Lang>,
    ) -> Self {
        use crate::deps::graph::ImportKind::{Glob, StarFrom};
        let segments = _module_segments(raw.raw_path.as_deref().unwrap_or(&raw.spec));
        let binds = if matches!(raw.kind, Glob | StarFrom) {
            Bind::OnDemand
        } else if let Some(alias) = raw.local_name.clone() {
            Bind::Name(alias)
        } else if lang.is_some_and(_imports_a_namespace) {
            // C# `using Example.Api;` names a namespace, not a type called
            // `Api`, and the enclosing namespace's own types win over it —
            // which is what `OnDemand` means to the resolver.
            Bind::OnDemand
        } else if lang.is_some_and(_specifier_ends_at_the_item) {
            segments.last().cloned().map_or(Bind::Module, Bind::Name)
        } else {
            Bind::Module
        };
        // A specifier that ends at the item names a module one segment
        // shorter: Python's `from .base import TextCodec` reaches the file
        // `base`, not a `TextCodec` inside it.
        let module_spec = match &binds {
            // Drop the specifier's own last segment, which is the item and
            // not a directory: `from .base import TextCodec` reaches the
            // file `base`. Keyed on the specifier's shape rather than on
            // the bound name, so a rename (`… import Codec as C`) still
            // drops `Codec`.
            Bind::Name(_) => raw
                .spec
                .rsplit_once('/')
                .map(|(head, _)| head)
                .unwrap_or(&raw.spec),
            _ => &raw.spec,
        };
        Self {
            binds,
            line: raw.line,
            resolved: _resolve_relative(module_spec, from),
            segments,
        }
    }
}

/// Does this extension name a source file rather than part of a module's
/// own name? A specifier's extension is dropped only for these, so a module
/// directory called `v1.2` keeps its name.
fn _is_a_source_extension(ext: &str) -> bool {
    matches!(
        ext,
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" | "py" | "rs"
    )
}

/// Is a module in this language a file rather than a declaration?
///
/// Such a language writes no namespace a qualifier could be checked
/// against, so a qualified reference there is a module path or a local
/// alias — evidence the resolver can use, never evidence it can reject on.
fn _module_is_a_file(lang: &str) -> bool {
    matches!(lang, "python" | "typescript" | "rust")
}

/// Can an import in this language bind a name other than the item's own?
///
/// Java is the one that cannot: it has no import rename in any form, so a
/// Java file never needs its imports extracted for the rename path — which
/// is a fact about the language rather than a guess about the text, and it
/// is what keeps a large Java tree from paying for the loose probes above.
fn _can_rename_an_import(lang: crate::deps::resolver::build::Lang) -> bool {
    !matches!(lang, crate::deps::resolver::build::Lang::Java)
}

/// Does an ordinary import in this language name a namespace to search
/// rather than the item it imports? C# is the one that does: every plain
/// `using` is a namespace import, and a specific type arrives only through
/// `using Alias = A.B.C`, which the alias branch above already handles.
fn _imports_a_namespace(lang: crate::deps::resolver::build::Lang) -> bool {
    matches!(lang, crate::deps::resolver::build::Lang::CSharp)
}

/// Does this language's import specifier spell out the item it imports, so
/// that its last segment is a type name rather than a module name?
///
/// `import org.postgresql.api.codec.TextCodec` does; `import { Base } from
/// './base'` does not, and reading `base` as the bound type there would
/// keep the import from matching anything.
fn _specifier_ends_at_the_item(lang: crate::deps::resolver::build::Lang) -> bool {
    use crate::deps::resolver::build::Lang::*;
    matches!(lang, Java | Kotlin | Scala | Rust | Python | Php)
}

/// Where `spec` points when it is relative to the file that wrote it. The
/// extractors normalise every language's relative form to `./x` / `../x`,
/// so one rule covers Python, TypeScript and JavaScript alike.
fn _resolve_relative(spec: &str, from: &Path) -> Option<PathBuf> {
    if !spec.starts_with('.') {
        return None;
    }
    let mut dir = from.parent()?.to_path_buf();
    let mut rest = spec;
    while let Some(r) = rest.strip_prefix("../") {
        dir.pop();
        rest = r;
    }
    let rest = rest.strip_prefix("./").unwrap_or(rest);
    for seg in rest.split('/').filter(|s| !s.is_empty() && *s != ".") {
        dir.push(seg);
    }
    // NodeNext TypeScript writes the *emitted* extension — `./base.js` for
    // a declaration that lives in `base.ts` — and the comparison is against
    // a path with its extension already stripped, so strip one here too.
    if dir
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(_is_a_source_extension)
    {
        dir.set_extension("");
    }
    Some(dir)
}

/// Is `outer` the same namespace as `inner`, or an ancestor of it? The
/// comparison is on a dot boundary, so `A` encloses `A.B` and `Ax` does not.
fn _encloses(outer: &str, inner: &str) -> bool {
    !outer.is_empty()
        && (inner == outer
            || inner
                .strip_prefix(outer)
                .is_some_and(|rest| rest.starts_with('.')))
}

/// The line range of the innermost namespace containing `line`, if any.
fn _innermost_namespace(decls: &[Declaration], line: usize) -> Option<(usize, usize)> {
    for d in decls {
        if d.kind != DeclarationKind::Namespace || line < d.start_line || line > d.end_line {
            continue;
        }
        return _innermost_namespace(&d.children, line).or(Some((d.start_line, d.end_line)));
    }
    None
}

/// The declarations a reference written on `from` may even consider.
///
/// A supertype reference never crosses a language: a Java `extends` clause
/// cannot name a TypeScript interface, whatever the two call themselves.
/// The name index is language-blind because a *target* may legitimately
/// span languages, so the bound belongs here — and it belongs in one place,
/// because the rename path does its own lookup and inherited the unfiltered
/// candidates when this test lived at the other call site.
fn _candidates_for(
    types: &[TypeEntry<'_>],
    index: &HashMap<String, Vec<usize>>,
    from: usize,
    name: &str,
) -> Vec<usize> {
    let lang = types[from].lang;
    index
        .get(name)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .copied()
        .filter(|&c| c != from && types[c].lang == lang)
        .collect()
}

/// What a supertype reference resolves to.
enum BaseRef {
    /// Exactly one declaration in the walk — an edge.
    One(usize),
    /// Several, with nothing in the referring file to choose between them.
    /// Not an edge: guessing would attach the subtype to a hierarchy it has
    /// no relation to.
    Several(Vec<usize>),
    /// Nothing in the walk declares this name — an external supertype
    /// (`extends Exception`), which is the common case, not a problem.
    None,
}

/// Resolve the supertype reference `base`, written on the type at `from`.
///
/// Each rule below is a *narrowing* applied only when it leaves something
/// behind: a repository where the name is unique resolves on the last rule
/// alone, exactly as the bare-name match used to, so the rules can only
/// change an answer that was a guess to begin with.
fn _resolve_base(
    imports: &ImportIndex<'_>,
    types: &[TypeEntry<'_>],
    index: &HashMap<String, Vec<usize>>,
    from: usize,
    base: &str,
) -> BaseRef {
    let simple = _normalize_type_name(base);
    let lang = types[from].lang;

    // A rename is the strongest evidence there is, because it is local and
    // explicit: `import a.b.Base as Alias` makes `Alias` mean `a.b.Base`
    // inside this file, whatever some other package declares under that
    // name. It therefore runs before the declaration index, not after it.
    if let Some(renamed) = _resolve_rename(imports, types, index, from, &simple) {
        // …except in C#, which looks in the enclosing namespace before its
        // using aliases. Kotlin, Rust and Scala read it the other way round,
        // so this is one language's rule rather than a general one.
        // C# consults each enclosing namespace's members innermost-first,
        // so a type in an *ancestor* namespace outranks the alias too.
        let own_scope_first = lang == "csharp"
            && _candidates_for(types, index, from, &simple)
                .iter()
                .any(|&c| _encloses(&types[c].package, &types[from].package));
        if !own_scope_first {
            return renamed;
        }
    }
    if !index.contains_key(&simple) {
        // No declaration carries this name: an external supertype, which is
        // the common case rather than a problem.
        return BaseRef::None;
    }
    let mut live = _candidates_for(types, index, from, &simple);
    if live.is_empty() {
        return BaseRef::None;
    }

    // 1. The reference is qualified. This runs before the single-candidate
    //    shortcut below and has to: `implements java.lang.Runnable` written
    //    where the walk declares one unrelated `mylib.Runnable` names the
    //    JDK's, and handing it the only candidate would be the merge this
    //    resolver exists to stop, with no second candidate to make it
    //    visible.
    //
    //    But a qualifier only *rules out* a declaration the language gives
    //    it something to be compared with. Where a module is a file rather
    //    than a declaration — Python, TypeScript, Rust — `api.Base` is as
    //    likely a module alias as a package path, and rejecting on it
    //    deleted the edge with no ambiguity to report. Such a qualifier says
    //    nothing and leaves the reference to the rules below, which is also
    //    why more than one survivor falls through rather than ending the
    //    resolution here.
    //
    //    The test is the language, not an empty `package` string: a Java
    //    file in the default package has no package clause and is still a
    //    Java file, and letting it through here brought back exactly the
    //    merge rule 1 exists to stop.
    let mut segments = _type_ref_segments(base);
    if lang == "php" && segments.first().is_some_and(|s| s == "namespace") {
        // PHP's `namespace\Root` means "Root in this file's namespace", the
        // way `self::` does elsewhere. The keyword is reserved, so a leading
        // segment spelling it is never a real qualifier.
        segments.remove(0);
    }
    if segments.len() > 1 {
        let expanded = _expand_alias_qualifier(imports, types, from, &mut segments);
        live.retain(|&c| _qualifiers_agree(&types[c], &segments) || _module_is_a_file(lang));
        // An expanded alias names a module the file spelled out, so where
        // the language has no namespace to compare it with, compare it with
        // the file. The import itself is the best evidence available: a
        // relative one knows the exact path it lands on, which is what
        // separates `./base` from every other `base.py` in the tree.
        //
        // It is evidence that stands whether or not it finds anything, for
        // the reason a rename does: the head is bound to another name, so
        // `api::Base` under `use external::stuff as api` is
        // `external::stuff::Base` and no declaration this walk holds under
        // some other module can be it. Keeping the candidates a rejecting
        // rule left behind sent that reference to whichever unrelated
        // `Base` the walk happened to declare.
        if let Some(HeadExpansion::Renamed(alias)) = &expanded {
            live.retain(|c| _import_names(&types[*c], alias, &simple));
        }
        match live.len() {
            0 => return BaseRef::None,
            1 => return BaseRef::One(live[0]),
            _ => {}
        }
    }
    if live.len() == 1 {
        // The overwhelmingly common case, and the one the bare-name match
        // always got right. Nothing below can change it, so skip the work —
        // in particular, do not re-read the file for its imports.
        return BaseRef::One(live[0]);
    }

    // 2. Declared in the same file.
    let file = types[from].file;
    _narrow(&mut live, |c| types[*c].file == file);
    if live.len() == 1 {
        return BaseRef::One(live[0]);
    }

    // 3. Named by an explicit single-type import. This outranks the
    //    referring file's own package, and has to: `org.postgresql.jdbc.codec`
    //    declares `TextCodec` and its members still import
    //    `org.postgresql.api.codec.TextCodec`, which is the one they mean
    //    (JLS 6.4.1 — a single-type import shadows a type of the same name
    //    from the compilation unit's package).
    let file_imports = imports.visible_from(file, types[from].decl.start_line);
    _narrow_by_imports(&mut live, types, &file_imports, &simple, false);
    if live.len() == 1 {
        return BaseRef::One(live[0]);
    }

    // 4. Declared in the same package — Java's own rule, and C#'s, and
    //    Kotlin's. A file with no package clause has none, and an empty
    //    package must not merge every such file into one scope.
    let package = &types[from].package;
    if !package.is_empty() {
        _narrow(&mut live, |c| &types[*c].package == package);
        if live.len() == 1 {
            return BaseRef::One(live[0]);
        }
    }

    // 5. Named by an on-demand import (`import a.b.*`), which ranks below
    //    the file's own package for the same reason it does in the language.
    _narrow_by_imports(&mut live, types, &file_imports, &simple, true);

    // 6. Nothing else declares the name.
    if live.len() == 1 {
        BaseRef::One(live[0])
    } else {
        BaseRef::Several(live)
    }
}

/// Resolve a name the referring file renamed on import: `import a.b.Base
/// as Alias` makes `class Child : Alias` a subtype of `a.b.Base`, which a
/// lookup for `Alias` can never find.
///
/// `None` means no import of this file binds the name, so the caller
/// carries on with the declaration index. `Some` means a rename claims it,
/// and its answer stands even where a declaration of the same name exists
/// elsewhere — the rename shadows it, in the resolver as in the language.
///
/// A rename whose target is outside the walk still claims the name:
/// `Some(BaseRef::None)`, not `None`. The distinction is the whole point of
/// the rule. An external dependency is normally *not* in the walk, so
/// falling through there sent the alias to whatever unrelated declaration
/// happened to carry the same simple name — the silent merge this resolver
/// exists to stop, in the case where it is most likely to happen.
fn _resolve_rename(
    imports: &ImportIndex<'_>,
    types: &[TypeEntry<'_>],
    index: &HashMap<String, Vec<usize>>,
    from: usize,
    simple: &str,
) -> Option<BaseRef> {
    let file = types[from].file;
    if !imports.may_rename(file) {
        return None;
    }
    let want = Bind::Name(simple.to_string());
    let mut live: Vec<usize> = Vec::new();
    let mut bound = false;
    for imp in imports
        .visible_from(file, types[from].decl.start_line)
        .iter()
        .filter(|i| i.binds == want)
    {
        let Some(real) = imp.segments.last() else {
            continue;
        };
        // The list is innermost-scope first, so this first binding of the
        // name is the one in force and every later one is an enclosing
        // scope's, shadowed. Which means the shadow rule is about the
        // binding, not about whether it renames: skipping past an ordinary
        // import to look for a rename let an outer `use other::Widget as
        // Root` claim a name an inner `use api::Root` had already taken, and
        // left two C# `using Root = …` aliases — the shape that exists
        // precisely to say which same-named type is meant — deciding
        // nothing between them.
        if real == simple {
            // Not a rename, so this rule has no answer; but it has settled
            // which import speaks for the name, and `_narrow_by_imports`
            // reads the same ordering to find it.
            return None;
        }
        bound = true;
        for c in _candidates_for(types, index, from, real) {
            if _import_names(&types[c], imp, real) {
                live.push(c);
            }
        }
        break;
    }
    live.sort_unstable();
    live.dedup();
    match live.len() {
        0 if bound => Some(BaseRef::None),
        0 => None,
        1 => Some(BaseRef::One(live[0])),
        _ => Some(BaseRef::Several(live)),
    }
}

/// Replace a leading qualifier segment that some import of the file binds
/// with the path that import spells out.
///
/// Two shapes need it, and they are the same operation. A module alias:
/// `using Api = Example.Other;` makes `Api.Base` mean
/// `Example.Other.Base`. And an ordinary import of an enclosing type:
/// `import org.example.TestClass` makes `TestClass.Nested.Sub` mean
/// `org.example.TestClass.Nested.Sub` — the partial spelling reads as a
/// trailing run of any same-shaped nesting until the head is expanded, so
/// two such types in different packages were reported as ambiguous when
/// the import had pinned one of them all along.
fn _expand_alias_qualifier(
    imports: &ImportIndex<'_>,
    types: &[TypeEntry<'_>],
    from: usize,
    segments: &mut Vec<String>,
) -> Option<HeadExpansion> {
    let file = types[from].file;
    let head = Bind::Name(segments[0].clone());
    let import = imports
        .visible_from(file, types[from].decl.start_line)
        .into_iter()
        .find(|i| i.binds == head)?;
    let renamed = import.segments.last() != segments.first();
    segments.splice(..1, import.segments.clone());
    Some(if renamed {
        HeadExpansion::Renamed(import)
    } else {
        HeadExpansion::Outer
    })
}

/// What an import contributed to a qualifier's leading segment.
enum HeadExpansion {
    /// The head was a rename, so the import itself is the best evidence
    /// for what the reference means — a relative one knows the exact file.
    Renamed(FileImport),
    /// The head was an ordinary import of an enclosing type. Nothing more
    /// is needed: the qualifier is absolute now and speaks for itself.
    Outer,
}

/// Narrow to the candidates some import of the referring file could be
/// naming. `on_demand` selects which half of the import list to consult:
/// the two halves are separate rules with the package rule between them.
///
/// One binding of the name, not every visible one. `imports` arrives
/// innermost-scope first, so the first import that binds the name is the one
/// in force and the rest are enclosing scopes' — and this rule matches on
/// "some spec names it", which took both and left a C# file with an inner
/// `using Root = A.Root;` under an outer `using Root = B.Root;` unable to
/// decide between two candidates the language tells apart.
fn _narrow_by_imports(
    live: &mut Vec<usize>,
    types: &[TypeEntry<'_>],
    imports: &[FileImport],
    simple: &str,
    on_demand: bool,
) {
    let mut named = false;
    let specs: Vec<&FileImport> = imports
        .iter()
        .filter(|i| match &i.binds {
            Bind::OnDemand => on_demand,
            Bind::Module => !on_demand,
            Bind::Name(n) => !on_demand && n == simple && !std::mem::replace(&mut named, true),
        })
        .collect();
    if specs.is_empty() {
        return;
    }
    _narrow(live, |c| {
        specs.iter().any(|i| _import_names(&types[*c], i, simple))
    });
}

/// Keep the candidates `keep` accepts, unless it accepts none — a rule that
/// rejects everything has nothing to say about which candidate is meant.
fn _narrow(live: &mut Vec<usize>, keep: impl Fn(&usize) -> bool) {
    if live.len() > 1 && live.iter().any(&keep) {
        live.retain(keep);
    }
}

/// The declarations a target names. An unqualified target names every
/// declaration of that simple name — the answer then covers all of them and
/// says so. A qualified one names only what the qualifier fits, and fitting
/// nothing is a rejected query rather than an answer for everything:
/// `implements nosuch.pkg.Widget` must not quietly answer for `a.b.Widget`.
fn _resolve_target(
    types: &[TypeEntry<'_>],
    candidates: &[usize],
    segments: &[String],
) -> Vec<usize> {
    if segments.len() < 2 {
        return candidates.to_vec();
    }
    candidates
        .iter()
        .copied()
        .filter(|&c| _qualifiers_agree(&types[c], segments))
        .collect()
}

/// Does the supertype reference `base` name a target the walk cannot see?
///
/// The simple names have to match, and two qualified spellings have to
/// agree: `implements java.lang.Runnable` is not answered by a class
/// extending `mylib.Runnable`. A bare reference matches a qualified target
/// and the other way round, since neither side contradicts the other.
fn _names_external_target(
    base: &str,
    simple: &str,
    target_segments: &[String],
    need_qualifier: bool,
) -> bool {
    if _normalize_type_name(base) != simple {
        return false;
    }
    let segments = _type_ref_segments(base);
    if segments.len() < 2 {
        return !need_qualifier;
    }
    target_segments.len() < 2
        || _qualifier_matches(&segments.join("."), target_segments)
        || _qualifier_matches(&target_segments.join("."), &segments)
}

/// The path an inheritance clause spells for `simple`, once the imports of
/// the file it was written in have had their say. `None` when the clause is
/// about some other type entirely.
///
/// An import speaks in two shapes, and the clause text shows neither. It
/// renames the type: `import com.api.Base as Alias` makes `Alias` the whole
/// clause. Or it binds the qualifier's leading segment: `use
/// external::stuff as api` makes `api::Handle` mean
/// `external::stuff::Handle`. Both are a local rebinding, so both are read
/// here rather than at one of the two call sites.
fn _spelled_reference(
    imports: &ImportIndex<'_>,
    types: &[TypeEntry<'_>],
    from: usize,
    base: &str,
    simple: &str,
) -> Option<Vec<String>> {
    let written = _normalize_type_name(base);
    let file = types[from].file;
    if written == simple {
        let mut segments = _type_ref_segments(base);
        // An unqualified clause has no head to expand, and a file that
        // renames nothing cannot have bound one — which is what keeps a
        // large Java tree from being reparsed for a rule it has no syntax
        // for.
        if segments.len() > 1 && imports.may_rename(file) {
            _expand_alias_qualifier(imports, types, from, &mut segments);
        }
        return Some(segments);
    }
    if !imports.may_rename(file) {
        return None;
    }
    let want = Bind::Name(written);
    imports
        .visible_from(file, types[from].decl.start_line)
        .into_iter()
        .find(|i| i.binds == want && i.segments.last().map(String::as_str) == Some(simple))
        .map(|i| i.segments)
}

/// Does the inheritance clause reach the out-of-walk target through an
/// import? `import com.api.Base as Alias` followed by `class Child : Alias`
/// implements `com.api.Base`, and comparing the raw clause text alone never
/// sees it — nor does it see `api::Handle` where `api` is bound to
/// `external::stuff`.
///
/// Only a spelling the import *changed* is answered here; anything the
/// clause text already says is [`_names_external_target`]'s, which is what
/// keeps this from re-admitting the bare references a qualified target is
/// meant to exclude.
fn _import_names_external_target(
    imports: &ImportIndex<'_>,
    types: &[TypeEntry<'_>],
    from: usize,
    base: &str,
    target_simple: &str,
    target_segments: &[String],
) -> bool {
    let Some(segments) = _spelled_reference(imports, types, from, base, target_simple) else {
        return false;
    };
    if segments == _type_ref_segments(base) {
        return false; // Already handled by the plain comparison.
    }
    target_segments.len() < 2
        || _qualifier_matches(&segments.join("."), target_segments)
        || _qualifier_matches(&target_segments.join("."), &segments)
}

/// The distinct qualified spellings a bare external target answered for.
///
/// `implements Runnable` over a tree where one class writes
/// `java.lang.Runnable` and another writes `otherlib.Runnable` covers two
/// unrelated types, and no declaration in the walk can say they are one.
/// Merging them silently is the defect this resolver exists to stop, so the
/// answer says which spellings it merged. A single spelling, or none beyond
/// the bare name, is not a qualification and reports nothing.
fn _external_spellings(
    imports: &ImportIndex<'_>,
    types: &[TypeEntry<'_>],
    simple: &str,
    direct: &[usize],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for &d in direct {
        for b in &types[d].decl.bases {
            // A clause may reach the target under a name the file renamed,
            // in which case the import holds the spelling and the clause
            // itself says only `Alias`. Those are claims too: `a.External
            // as A` and `b.External as B` name two different types.
            let spelled = _spelled_reference(imports, types, d, b, simple);
            let Some(segments) = spelled else { continue };
            // Only a qualified spelling makes a claim. A bare `Runnable`
            // sitting next to `java.lang.Runnable` is ordinary mixed style
            // in one Java file, not two types — counting it would fire this
            // note on most repositories and advise a split that drops half
            // the answer.
            if segments.len() > 1 {
                out.push(segments.join("."));
            }
        }
    }
    out.sort();
    out.dedup();
    // Two qualified spellings that agree are one claim spelled at different
    // depths (`codec.TextCodec` and `org.postgresql.codec.TextCodec`). They
    // need not be neighbours after sorting, so each is compared against the
    // ones already kept rather than against the previous element alone.
    let mut kept: Vec<String> = Vec::new();
    for spelling in out {
        let segments = _type_ref_segments(&spelling);
        if !kept.iter().any(|k| {
            _qualifier_matches(k, &segments)
                || _qualifier_matches(&spelling, &_type_ref_segments(k))
        }) {
            kept.push(spelling);
        }
    }
    let out = kept;
    if out.len() > 1 {
        out
    } else {
        Vec::new()
    }
}

/// Can `segments` — a qualified target, or a qualified supertype reference —
/// be naming this declaration?
///
/// Either name can be the longer one. A Java qname is absolute, so
/// `jdbc.codec.TextCodec` is checked as a suffix of it. A Rust or Python
/// declaration knows no package, so `crate::net::Client` is longer than the
/// qname `Client`, and the segments the qname does not account for have to
/// show up in the path of the file that declares it — otherwise every
/// invented qualifier would match every bare name.
fn _qualifiers_agree(t: &TypeEntry<'_>, segments: &[String]) -> bool {
    let have: Vec<&str> = t.qname.split('.').collect();
    if have.len() >= segments.len() {
        return have[have.len() - segments.len()..]
            .iter()
            .zip(segments)
            .all(|(a, b)| a == b);
    }
    let (extra, tail) = segments.split_at(segments.len() - have.len());
    if !tail.iter().zip(&have).all(|(a, b)| a == b) {
        return false;
    }
    let in_path = |seg: &String| {
        t.path
            .components()
            .any(|c| Path::new(c.as_os_str()).file_stem() == Some(std::ffi::OsStr::new(seg)))
    };
    extra
        .iter()
        .all(|seg| matches!(seg.as_str(), "crate" | "super" | "self") || in_path(seg))
}

/// Does `qname` end with the segments the reference spelled out? A bare
/// reference (one segment) matches everything, so callers apply this only
/// as a narrowing.
fn _qualifier_matches(qname: &str, segments: &[String]) -> bool {
    if segments.len() < 2 {
        return false;
    }
    let have: Vec<&str> = qname.split('.').collect();
    have.len() >= segments.len()
        && have[have.len() - segments.len()..]
            .iter()
            .zip(segments)
            .all(|(a, b)| a == b)
}

/// Append `name` to the qualified name it sits under, in the one spelling
/// every rule here compares against: dot-separated segments.
///
/// `name` arrives as its language spells it, and two languages spell a
/// namespace with more than one segment in a single declaration — PHP's
/// `namespace App\Base` and C++17's `namespace A::B`. Keeping that
/// spelling left the declaration one segment where a reference to it split
/// into three, so `_qualifiers_agree` could never agree and the edge
/// disappeared without even being reported as ambiguous. Splitting on the
/// same separators as a type reference is what puts both sides in one
/// alphabet; it also drops the generic arguments a declared name may carry,
/// which is what the name index already does to its keys.
fn _join_dot(prefix: &str, name: &str) -> String {
    let name = _type_ref_segments(name).join(".");
    match (prefix.is_empty(), name.is_empty()) {
        (true, _) => name,
        (_, true) => prefix.to_string(),
        _ => format!("{}.{}", prefix, name),
    }
}

/// Could `imp`, written in the referring file, be naming `t`?
///
/// The comparison is against whatever the declaration knows about itself. A
/// declaration inside a namespace knows its qualified name, and the spec
/// has to agree with it — `import org.postgresql.api.codec.TextCodec` does
/// not name `org.postgresql.jdbc.codec.TextCodec`, and a looser check that
/// let a shared directory segment count was how the wrong one got picked. A
/// declaration in a language whose module is a file (Python, TypeScript)
/// knows no namespace, so the spec is matched against the file instead.
fn _import_names(t: &TypeEntry<'_>, imp: &FileImport, simple: &str) -> bool {
    // The path the import spells out, extended to the type it would name:
    // `import a.b.*` and `import { Foo } from './a'` both stop short of it.
    // A specifier that already ends at the item needs nothing appended; one
    // that names a module (`from './base'`) does. The test is whether the
    // specifier's own last segment is the item, not whether the import binds
    // that name — a rename binds `Alias` while still spelling out `a.b.Base`.
    let mut full = imp.segments.clone();
    let ends_at_item =
        matches!(&imp.binds, Bind::Name(_)) && full.last().map(String::as_str) == Some(simple);
    if !ends_at_item {
        full.push(simple.to_string());
    }

    if !t.package.is_empty() {
        // Either name may be the shorter one: a Java qname is absolute
        // while a Rust `mod` chain is relative to a crate root the spec
        // still spells out.
        let have: Vec<String> = t.qname.split('.').map(str::to_string).collect();
        return _qualifier_matches(&t.qname, &full) || _qualifier_matches(&full.join("."), &have);
    }

    if let Some(dir) = &imp.resolved {
        // A relative specifier names one file, so compare paths: `./base`
        // written in `impl/leaf.ts` is `impl/base.ts` and nothing else.
        // `index.ts` / `__init__.py` / `mod.rs` stand for their directory.
        return t.path.with_extension("") == *dir || t.path.parent() == Some(dir.as_path());
    }
    // An absolute specifier is matched against the file's own path from the
    // right. Requiring two segments to line up is what separates
    // `impl.base` from `api/base.py`; requiring only the last would call
    // every `base.py` in the repository a match. A one-segment specifier
    // has only one segment to offer and stays a weak signal.
    //
    // `crate`, `super` and `self` anchor a path without naming a directory,
    // so they are dropped first: `use crate::a::Base` has one directory
    // segment to offer, not two, and counting `crate` would sink it.
    let module: Vec<&String> = full[..full.len() - 1]
        .iter()
        .filter(|s| !matches!(s.as_str(), "crate" | "super" | "self"))
        .collect();
    let have = _path_segments(t.path);
    let common = module
        .iter()
        .rev()
        .zip(have.iter().rev())
        .take_while(|(a, b)| ***a == **b)
        .count();
    common >= module.len().min(2)
}

/// A file's path as module segments: directory names plus the file stem.
fn _path_segments(path: &Path) -> Vec<String> {
    let mut out: Vec<String> = path
        .parent()
        .into_iter()
        .flat_map(|p| p.components())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if let Some(stem) = path.file_stem() {
        out.push(stem.to_string_lossy().into_owned());
    }
    out
}

/// Split an import specifier into segments. Unlike a type reference a
/// specifier can be a path (`./helpers`, `com/foo/Bar`), so slashes
/// separate too.
fn _module_segments(spec: &str) -> Vec<String> {
    spec.replace("::", ".")
        .split(['.', '/', '\\'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Split a type reference into its path segments, dropping generic
/// arguments and array brackets: `a.b.Map<K, V>` → `["a", "b", "Map"]`.
fn _type_ref_segments(name: &str) -> Vec<String> {
    let mut name = name.trim();
    if let Some(i) = name.find('<') {
        name = &name[..i];
    }
    if let Some(i) = name.find('[') {
        name = &name[..i];
    }
    name.replace("::", ".")
        .split(['.', '\\'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn _impl_match(path: &Path, d: &Declaration, via: Vec<String>) -> ImplMatch {
    ImplMatch {
        path: path.to_string_lossy().to_string(),
        start_line: d.start_line,
        kind: d.kind.to_string(),
        name: d.name.clone(),
        via,
    }
}

/// The simple name a type reference ends in: `a.b.Map<K, V>` → `Map`.
///
/// Every separator the supported languages use has to be cut, including
/// PHP's backslash — leaving it in made `implements \\Ns\\Root` a name no
/// declaration index could ever carry, which is most PHP inheritance.
fn _normalize_type_name(name: &str) -> String {
    _type_ref_segments(name).pop().unwrap_or_default()
}

#[derive(Serialize)]
struct JsonImplementsDoc<'a> {
    schema: &'static str,
    target: &'a str,
    transitive: bool,
    matches: &'a [ImplMatch],
    /// Supertype references left out of `matches` because the name they
    /// spelled is declared in more than one package. Absent when empty.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    ambiguous: &'a [AmbiguousBase],
    /// Present only when the target name itself matched several
    /// declarations; `matches` then covers all of them.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    target_candidates: &'a [BaseCandidate],
    /// Present only when a bare out-of-walk target covered more than one
    /// qualified spelling.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    external_spellings: &'a [String],
}

/// Render `implements --json`.
pub fn render_json_implements(
    target: &str,
    report: &ImplementsReport,
    transitive: bool,
    pretty: bool,
) -> String {
    let doc = JsonImplementsDoc {
        schema: JSON_SCHEMA_IMPLEMENTS,
        target,
        transitive,
        matches: &report.matches,
        ambiguous: &report.ambiguous,
        target_candidates: &report.target_candidates,
        external_spellings: &report.external_spellings,
    };
    crate::core::_to_json(&doc, pretty)
}
