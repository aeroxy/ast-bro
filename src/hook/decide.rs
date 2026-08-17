use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::event::{Channel, Decision, ToolCallEvent};
// The ceiling and the room the channel notice takes out of it both belong to the
// module that assembles a payload; what this one needs is the budget that leaves.
use super::io::{MAP_BUDGET, MAX_MAP_BYTES};
use crate::core::{MapUnit, MapUnitKind};

#[derive(Debug, Clone)]
pub struct DecideOpts {
    pub min_lines: usize,
    pub always: bool,
}

pub fn decide(event: &ToolCallEvent, opts: &DecideOpts, channel: Channel) -> Decision {
    if event.tool_name != "Read" {
        return Decision::PassThrough;
    }
    let Some(path) = event.file_path.as_deref() else {
        return Decision::PassThrough;
    };
    // A range request is the caller asking for source, so substituting would
    // withhold what they asked for — unless the read already failed, in which
    // case it returned no source and the map is purely additive. The host's own
    // refusal tells the agent to retry with a range, so gating that retry out
    // would leave the hole this event exists to close.
    if event.has_offset_or_limit && !channel.is_additive() {
        return Decision::PassThrough;
    }
    if !crate::main_helpers::can_parse_for_hook(path) {
        return Decision::PassThrough;
    }
    if !opts.always && !channel.is_additive() {
        match line_count_at_least(path, opts.min_lines) {
            Ok(true) => {}
            _ => return Decision::PassThrough,
        }
    }
    match render_map_for(path) {
        Some(map) => Decision::Substitute { map },
        None => Decision::PassThrough,
    }
}

fn line_count_at_least(path: &Path, threshold: usize) -> std::io::Result<bool> {
    use std::io::{BufRead, BufReader};
    let meta = std::fs::metadata(path)?;
    if (meta.len() as usize) < threshold {
        return Ok(false);
    }
    let f = std::fs::File::open(path)?;
    let r = BufReader::new(f);
    let mut count = 0usize;
    for line in r.lines() {
        line?;
        count += 1;
        if count >= threshold {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Detail the hook sheds, in order, to bring a map under [`MAP_BUDGET`].
///
/// Shedding beats truncating because the reader needs every declaration: a map
/// that stops halfway reads as a file that ends halfway, and they act on the
/// absence. The order sheds what costs no declarations first — doc comments and
/// attributes are detail about a declaration, private items and fields are
/// declarations.
///
/// Measured on Apache Calcite's `SqlFunctions.java`, the largest map in that
/// repository at 7771 hand-written lines: 142,692 bytes in full, 74,607 without
/// docs or attributes with all 920 declarations intact, 64,146 once private items
/// and fields go, keeping 799. No level under the budget keeps all 920, which is
/// why the payload names what it shed and where to get the rest.
fn map_levels() -> [(&'static str, crate::core::MapOptions); 4] {
    use crate::core::MapOptions;
    let lossless = MapOptions {
        include_docs: false,
        include_attributes: false,
        ..MapOptions::default()
    };
    let lean = MapOptions {
        include_private: false,
        include_fields: false,
        ..lossless.clone()
    };
    let leanest = MapOptions {
        max_members: Some(50),
        ..lean.clone()
    };
    [
        ("", MapOptions::default()),
        ("doc comments and attributes", lossless),
        ("doc comments, attributes, fields, and private items", lean),
        (
            "doc comments, attributes, fields, private items, and members past 50 per type",
            leanest,
        ),
    ]
}

fn render_map_for(path: &Path) -> Option<String> {
    let res = crate::main_helpers::parse_file_for_hook(path)?;
    let levels = map_levels();
    // Rendered lazily and kept: a level that overflows needs the leaner levels to
    // decide what to keep, and the loop reaches those levels next.
    let mut rendered: Vec<Vec<MapUnit>> = Vec::with_capacity(levels.len());
    let mut full_bytes = 0usize;
    // Which level to spend the budget on is decided by the declarations it
    // delivers, not by which one fits first: a file of 2900 private and 100
    // public functions fits once private items go, and that "success" hands over
    // 100 declarations while leaving 93% of the budget unused, where trimming the
    // level before it carries 1871 — the same 100 publics and 1771 of the
    // helpers. How the budget is then spent *within* a level is `cap_map`'s rule
    // and a different one: it funds by rank, which on a file of wide type headers
    // and cheap members buys fewer declarations than file order would, and buys
    // the right ones.
    let mut best: Option<Candidate> = None;
    let mut required: Option<HashSet<usize>> = None;
    for (i, (shed, opts)) in levels.iter().enumerate() {
        if rendered.len() == i {
            rendered.push(crate::core::render_map_units(&res, opts));
        }
        if i == 0 {
            full_bytes = map_bytes(&rendered[0]);
        }
        // Naming what is missing is the whole point: absence of a private helper
        // from this map must not read as absence from the file. The note is built
        // before the fit decision so it counts against the budget.
        let note = if shed.is_empty() {
            String::new()
        } else {
            shed_note(shed, path)
        };
        let whole = whole_payload(&rendered[i], &note);
        let fits = whole.is_some();
        let candidate = if let Some(payload) = whole {
            let delivered = entry_count(&rendered[i]);
            Candidate {
                payload,
                required: delivered,
                delivered,
            }
        } else {
            for (_, leaner) in levels.iter().skip(rendered.len()) {
                rendered.push(crate::core::render_map_units(&res, leaner));
            }
            // The same set at every level that reaches here, since the loop stops
            // at the first level that fits: a scan from any earlier level finds
            // that same one. Computed at the first, and reused.
            let required = required
                .get_or_insert_with(|| required_ids(&rendered[i + 1..], &levels[i + 1..], path));
            cap_map(
                &rendered[i],
                &rendered[i + 1..],
                required,
                shed,
                full_bytes,
                path,
            )
        };
        // Strictly greater, so the richest level wins a tie.
        if best.as_ref().is_none_or(|b| candidate.score() > b.score()) {
            best = Some(candidate);
        }
        if fits {
            // No later level can deliver more: every level after this one only
            // removes declarations. (Shedding docs and attributes removes none —
            // they live inside a declaration's unit rather than beside it — but
            // that level comes earlier, so it is already considered.) Stopping
            // here leaves the common case, a small file whose full map fits, at a
            // single render.
            break;
        }
    }
    let payload = best.map(|b| b.payload);
    debug_assert!(
        payload.as_ref().is_none_or(|p| p.len() <= MAP_BUDGET),
        "a payload left the hook over the budget"
    );
    payload
}

/// One level's answer, and what it costs to prefer another level's.
struct Candidate {
    payload: String,
    /// Declarations it carries that a whole leaner candidate would carry too.
    required: usize,
    /// Declarations it carries at all.
    delivered: usize,
}

impl Candidate {
    /// What a candidate is chosen by, richest first.
    ///
    /// Declarations decide it, but not before the public surface does. A trim
    /// that runs out of budget inside the top rank can still out-count the leaner
    /// candidate that fits whole — it skips the one unit too wide to fit and
    /// spends the remainder on cheaper ones a rank below — and that payload is
    /// missing declarations a cheaper answer would have delivered. Counting the
    /// required set first makes that trade impossible to win.
    fn score(&self) -> (usize, usize) {
        (self.required, self.delivered)
    }
}

/// The payload `units` and `note` make, if it is one the hook may deliver.
///
/// The size is taken from the assembled payload rather than from its parts:
/// [`with_note`] puts the note on a line of its own, and a fit decided without
/// that byte is a ceiling resting on how the renderer happened to end its last
/// unit. Assembling it costs a copy of the map, so the parts are added up first
/// — the payload is never smaller than they are — and a level already over the
/// ceiling by that measure is refused without the copy.
fn whole_payload(units: &[MapUnit], note: &str) -> Option<String> {
    if map_bytes(units) + note.len() > MAP_BUDGET {
        return None;
    }
    let out = with_note(join_units(units), note);
    (out.len() <= MAP_BUDGET).then_some(out)
}

/// The declarations a trimmed candidate must carry to be worth preferring.
///
/// They are the leanest whole answer available: the richest of the leaner levels
/// whose map fits, which is the level the loop in [`render_map_for`] would stop
/// at. Empty when no leaner level fits, where nothing is owed and the count
/// decides alone.
fn required_ids(
    leaner: &[Vec<MapUnit>],
    levels: &[(&'static str, crate::core::MapOptions)],
    path: &Path,
) -> HashSet<usize> {
    for (units, (shed, _)) in leaner.iter().zip(levels) {
        if whole_payload(units, &shed_note(shed, path)).is_some() {
            return declaration_ids(units);
        }
    }
    HashSet::new()
}

fn declaration_ids(units: &[MapUnit]) -> HashSet<usize> {
    units
        .iter()
        .filter_map(|u| match u.kind {
            MapUnitKind::Declaration { id } => Some(id),
            _ => None,
        })
        .collect()
}

/// The declarations a map carries, which is what one candidate is compared to
/// another by.
fn entry_count(units: &[MapUnit]) -> usize {
    units
        .iter()
        .filter(|u| matches!(u.kind, MapUnitKind::Declaration { .. }))
        .count()
}

/// The bytes `units` occupy once joined.
fn map_bytes(units: &[MapUnit]) -> usize {
    units.iter().map(|u| u.text.len()).sum::<usize>() + units.len().saturating_sub(1)
}

fn join_units(units: &[MapUnit]) -> String {
    units
        .iter()
        .map(|u| u.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Appends `note` to `map`, on its own line.
///
/// A map whose last unit is a declaration does not end in a newline — a file with
/// no declarations at all ends at its header — so the note has to be given one or
/// it reads as part of that line.
fn with_note(map: String, note: &str) -> String {
    if note.is_empty() {
        return map;
    }
    let mut out = map;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(note);
    out
}

/// The line a shed map ends with, naming what is gone and how to get it.
fn shed_note(shed: &str, path: &Path) -> String {
    format!(
        "# note: {shed} dropped to fit the {MAX_MAP_BYTES} byte cap; `ast-bro map {}` for the rest\n",
        path.display()
    )
}

/// How far down the ladder each unit of `units` survives.
///
/// A declaration that is still there at the leanest level is the one the reader
/// is least able to do without, so a trim spends the budget on it first. The
/// ladder already ranks detail by what it costs to lose ([`map_levels`]), and
/// this reads that ranking back off the renders rather than restating it.
///
/// Two units rank by inheritance instead. A `... +N more member(s)` marker takes
/// its type's rank, being a statement about that type, and the header and the
/// parse warning outrank everything: a map without them names no file and
/// claims a shape the parser did not manage to read.
fn survival_ranks(units: &[MapUnit], leaner: &[Vec<MapUnit>]) -> Vec<usize> {
    let sets: Vec<HashSet<usize>> = leaner
        .iter()
        .map(|render| declaration_ids(render))
        .collect();
    let mut by_decl: HashMap<usize, usize> = HashMap::new();
    let mut ranks = Vec::with_capacity(units.len());
    for unit in units {
        let rank = match unit.kind {
            MapUnitKind::Preamble => usize::MAX,
            MapUnitKind::Declaration { id } => {
                let rank = sets.iter().filter(|s| s.contains(&id)).count();
                by_decl.insert(id, rank);
                rank
            }
            // The parent is rendered ahead of what nests under it, so its rank is
            // already known.
            MapUnitKind::MoreMembers => unit
                .parent
                .and_then(|p| by_decl.get(&p).copied())
                .unwrap_or(0),
            MapUnitKind::Separator => 0,
        };
        ranks.push(rank);
    }
    ranks
}

/// Drops whole units from a map until it fits [`MAP_BUDGET`], saying what went.
///
/// The last resort, reached only when a level of detail still leaves the map too
/// big. A silently short map is worse than a trimmed one: the reader treats a map
/// as the file's whole shape and concludes a symbol does not exist, so the marker
/// names what was shed on the way here, how many entries went, and the file's
/// real size.
///
/// Three rules keep the result useful and truthful.
///
/// The budget is spent in [`survival_ranks`] order rather than front to back,
/// because a map's public surface is routinely its last declarations: filling in
/// file order delivers 1733 private items of one file and none of its 500 public
/// ones, which no reader would choose. Order within a rank is the file's own, and
/// so is the order the kept units are written back out in.
///
/// An over-wide unit is skipped rather than ending the scan, because one
/// generated signature with thousands of parameters would otherwise drop every
/// declaration after it.
///
/// And a dropped declaration takes everything nested under it, because a member
/// kept without its type header reads as a member of the previous type — a map
/// asserting a structure the file does not have. That rule needs its parent
/// decided first, and the funding order gives it that: `_render_decl` returns
/// before rendering anything for a declaration the options exclude, so a child
/// reaches a render only where every one of its ancestors did. A parent
/// therefore survives at least as far down the ladder as its members, its rank
/// is never lower, and a stable sort leaves the two in file order when the ranks
/// are equal.
fn cap_map(
    units: &[MapUnit],
    leaner: &[Vec<MapUnit>],
    required: &HashSet<usize>,
    shed: &str,
    full_bytes: usize,
    path: &Path,
) -> Candidate {
    // Size the marker against the widest numbers it can carry, so the budget it
    // comes out of is never too small; the real numbers are no wider, since what
    // is kept never exceeds the whole map and what is dropped never exceeds the
    // declaration count.
    // One more byte is held back for the newline the marker goes on.
    let budget = MAP_BUDGET
        .saturating_sub(
            trim_note(map_bytes(units), full_bytes, entry_count(units), shed, path).len(),
        )
        .saturating_sub(1);
    let ranks = survival_ranks(units, leaner);
    let mut order: Vec<usize> = (0..units.len()).collect();
    // Stable, which is what decides a parent before its members.
    order.sort_by_key(|&i| std::cmp::Reverse(ranks[i]));

    let mut keep = vec![false; units.len()];
    let mut dropped_parents: HashSet<usize> = HashSet::new();
    // The bytes the kept units occupy once joined, which is the number the marker
    // reports: a unit costs a newline only when it follows another.
    let mut len = 0usize;
    let mut kept_units = 0usize;
    let mut dropped = 0usize;
    for i in order {
        let unit = &units[i];
        let orphaned = unit.parent.is_some_and(|p| dropped_parents.contains(&p));
        let cost = unit.text.len() + usize::from(kept_units > 0);
        if !orphaned && len + cost <= budget {
            len += cost;
            kept_units += 1;
            keep[i] = true;
            continue;
        }
        if let MapUnitKind::Declaration { id } = unit.kind {
            dropped_parents.insert(id);
            dropped += 1;
        }
    }

    let kept: Vec<&str> = units
        .iter()
        .zip(&keep)
        .filter(|(_, k)| **k)
        .map(|(u, _)| u.text.as_str())
        .collect();
    let (mut delivered, mut kept_required) = (0usize, 0usize);
    for (unit, keep) in units.iter().zip(&keep) {
        if let (true, MapUnitKind::Declaration { id }) = (*keep, unit.kind) {
            delivered += 1;
            kept_required += usize::from(required.contains(&id));
        }
    }
    let note = trim_note(len, full_bytes, dropped, shed, path);
    let out = with_note(kept.join("\n"), &note);
    debug_assert!(out.len() <= MAP_BUDGET, "trim overshot the budget");
    Candidate {
        payload: out,
        required: kept_required,
        delivered,
    }
}

/// The line a trimmed map ends with. Separate from the cut so its width can be
/// measured before the cut is chosen.
fn trim_note(kept: usize, full_bytes: usize, dropped: usize, shed: &str, path: &Path) -> String {
    let shed_clause = if shed.is_empty() {
        String::new()
    } else {
        format!("{shed} dropped, then ")
    };
    format!(
        "# note: {shed_clause}map trimmed to {kept} bytes from a {full_bytes} byte full map{}; \
`ast-bro map {}` for the whole map, `ast-bro show {} <symbol>` for one body\n",
        if dropped > 0 {
            format!(", {dropped} entr{} dropped", if dropped == 1 { "y" } else { "ies" })
        } else {
            String::new()
        },
        path.display(),
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn opts() -> DecideOpts {
        DecideOpts {
            min_lines: 200,
            always: false,
        }
    }

    fn ev(tool: &str, path: Option<PathBuf>, offset: bool) -> ToolCallEvent {
        ToolCallEvent {
            tool_name: tool.to_string(),
            file_path: path,
            has_offset_or_limit: offset,
        }
    }

    fn preamble(text: &str) -> MapUnit {
        MapUnit {
            text: text.to_string(),
            kind: MapUnitKind::Preamble,
            parent: None,
        }
    }

    fn decl_unit(text: &str, id: usize, parent: Option<usize>) -> MapUnit {
        MapUnit {
            text: text.to_string(),
            kind: MapUnitKind::Declaration { id },
            parent,
        }
    }

    fn separator() -> MapUnit {
        MapUnit {
            text: String::new(),
            kind: MapUnitKind::Separator,
            parent: None,
        }
    }

    /// The channel a substitution replaces something on.
    fn replacing() -> Channel {
        Channel::Deny
    }

    /// The channel a substitution only adds to, after the read already failed.
    fn additive() -> Channel {
        Channel::Augment
    }

    #[test]
    fn pass_through_for_non_read_tool() {
        let d = decide(&ev("Bash", Some(PathBuf::from("a.rs")), false), &opts(), replacing());
        assert!(matches!(d, Decision::PassThrough));
    }

    #[test]
    fn pass_through_when_offset_or_limit_set() {
        let d = decide(&ev("Read", Some(PathBuf::from("a.rs")), true), &opts(), replacing());
        assert!(matches!(d, Decision::PassThrough));
    }

    #[test]
    fn pass_through_for_unsupported_extension() {
        let d = decide(
            &ev("Read", Some(PathBuf::from("img.png")), false),
            &opts(),
            replacing(),
        );
        assert!(matches!(d, Decision::PassThrough));
    }

    #[test]
    fn pass_through_when_below_threshold() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("small.rs");
        std::fs::write(&p, "fn main() {}\n").unwrap();
        let d = decide(&ev("Read", Some(p), false), &opts(), replacing());
        assert!(matches!(d, Decision::PassThrough));
    }

    #[test]
    fn substitutes_when_above_threshold() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("big.rs");
        let mut s = String::new();
        for i in 0..300 {
            s.push_str(&format!("fn f{}() {{}}\n", i));
        }
        std::fs::write(&p, &s).unwrap();
        let d = decide(&ev("Read", Some(p), false), &opts(), replacing());
        match d {
            // The map arrives undecorated: the notice that leads it is the
            // channel's business, and `io` is where it gets pinned.
            Decision::Substitute { map } => {
                assert!(map.contains("fn f"));
                assert!(
                    !map.contains("ast-bro substituted"),
                    "decide() must not frame the map:\n{map}"
                );
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    /// A read the host refused is substituted whatever its line count.
    ///
    /// The refusal counts bytes, not lines — a 90-line file of 355 KB trips
    /// it — so gating on lines would silence the map for exactly the reads that
    /// need it. Regression guard: this passed through before the gate was fixed.
    #[test]
    fn failed_read_substitutes_below_the_line_threshold() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("wide.rs");
        let pad = "x".repeat(4000);
        let mut src = String::new();
        for i in 0..20 {
            src.push_str(&format!("pub fn f{}() {{ let s = \"{}\"; let _ = s; }}\n", i, pad));
        }
        std::fs::write(&p, &src).unwrap();
        assert!(src.lines().count() < 200, "the fixture must be under the threshold");

        let e = ev("Read", Some(p.clone()), false);
        assert!(
            matches!(decide(&e, &opts(), replacing()), Decision::PassThrough),
            "a successful read of a short file is still passed through"
        );
        match decide(&e, &opts(), additive()) {
            Decision::Substitute { map } => assert!(map.contains("pub fn f0")),
            other => panic!("a failed read must be substituted: {other:?}"),
        }
    }

    /// A file the hook cannot parse has no map to offer, failed read or not.
    #[test]
    fn failed_read_still_needs_a_parseable_file() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("img.png");
        std::fs::write(&p, "not source").unwrap();
        let e = ev("Read", Some(p), false);
        assert!(matches!(decide(&e, &opts(), additive()), Decision::PassThrough));
    }

    /// A ranged retry of a read the host refused still gets the map.
    ///
    /// The refusal tells the agent to retry with `offset`/`limit`, and a slice of
    /// an over-large file can be refused again. Passing through there would hand
    /// back a second bare error — the hole this event exists to close. On a
    /// successful read the same range request is passed through, because there
    /// the caller is asking for source and gets it.
    #[test]
    fn failed_ranged_read_is_substituted_but_a_successful_one_is_not() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("big.rs");
        std::fs::write(&p, "fn f() {}\n".repeat(300)).unwrap();

        let e = ev("Read", Some(p), true);
        assert!(
            matches!(decide(&e, &opts(), replacing()), Decision::PassThrough),
            "a range on a successful read is the caller asking for source"
        );
        match decide(&e, &opts(), additive()) {
            Decision::Substitute { map } => assert!(map.contains("fn f")),
            other => panic!("a refused ranged read must still get the map: {other:?}"),
        }
    }

    /// A minified bundle is bounded by truncation, and says so.
    ///
    /// Its size is declaration count, so shedding detail cannot help: the ladder
    /// runs out and `cap_map` is the backstop. The marker has to name a route to
    /// the rest, because the names a reader would `show` are what got dropped.
    #[test]
    fn a_minified_bundle_is_trimmed_and_points_at_the_whole_map() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("bundle.rs");
        let src: String = (0..20000).map(|i| format!("pub fn f{i}() {{}} ")).collect();
        std::fs::write(&p, &src).unwrap();

        match decide(&ev("Read", Some(p.clone()), false), &opts(), additive()) {
            Decision::Substitute { map } => {
                assert!(
                    map.len() <= MAP_BUDGET,
                    "the budget counts the marker and the notice: {} bytes against {}",
                    map.len(),
                    MAP_BUDGET
                );
                assert!(map.contains("map trimmed to"), "no marker:\n{}", &map[..300]);
                assert!(map.contains("ast-bro map "), "no route to the rest");
                assert!(map.starts_with('#'), "the header line must survive");
            }
            other => panic!("expected a trimmed map: {other:?}"),
        }
    }

    /// A hand-written file too big for the full map keeps every declaration.
    ///
    /// The reader needs the declarations, not the doc comments, so detail is shed
    /// before anything is cut — and what was shed is named, or an absent private
    /// helper reads as one that does not exist. Regression guard: under a
    /// truncating-only cap this file lost most of its declarations.
    #[test]
    fn a_large_hand_written_file_sheds_detail_instead_of_declarations() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("wide.rs");
        let doc = "/// ".to_string() + &"documentation ".repeat(40);
        let mut src = String::new();
        for i in 0..900 {
            src.push_str(&format!("{doc}\npub fn f{i}(a: u32, b: u32) -> u32 {{ a + b }}\n"));
        }
        std::fs::write(&p, &src).unwrap();

        let full = crate::core::render_map(
            &crate::main_helpers::parse_file_for_hook(&p).unwrap(),
            &crate::core::MapOptions::default(),
        );
        assert!(
            full.len() > MAX_MAP_BYTES,
            "fixture must exceed the cap: {} bytes",
            full.len()
        );

        match decide(&ev("Read", Some(p), false), &opts(), additive()) {
            Decision::Substitute { map } => {
                assert!(map.len() <= MAP_BUDGET, "{} bytes", map.len());
                assert!(!map.contains("map trimmed to"), "must shed, not truncate");
                assert!(map.contains("doc comments"), "must name what it shed");
                for name in ["pub fn f0", "pub fn f899"] {
                    assert!(map.contains(name), "lost {name}: declarations must survive");
                }
            }
            other => panic!("expected a shed map: {other:?}"),
        }
    }

    /// A map under the cap is handed over byte for byte.
    #[test]
    fn a_map_under_the_cap_is_untouched() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("big.rs");
        std::fs::write(&p, "fn f() {}\n".repeat(300)).unwrap();
        match decide(&ev("Read", Some(p), false), &opts(), replacing()) {
            Decision::Substitute { map } => {
                assert!(map.len() < MAX_MAP_BYTES);
                assert!(!map.contains("more line(s) dropped"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// A whole map is measured assembled, because its parts do not show the line
    /// the marker takes.
    ///
    /// Every map a renderer produces ends in a newline — a top-level declaration
    /// is followed by a separator — so on any real file the two measures agree
    /// and the assembled check reads as redundant. It is not: on a map ending in
    /// a declaration, parts adding up to exactly the budget are delivered a byte
    /// over the ceiling. The three points are the budget on the nose, one byte
    /// under it, and the same map ending in a separator, which is the shape that
    /// hides the byte.
    #[test]
    fn a_map_is_measured_with_the_line_its_marker_takes() {
        let note = shed_note(map_levels()[1].0, Path::new("/tmp/x.rs"));
        let exact = MAP_BUDGET - note.len();

        // Filled to just under the target, then one unit sized to land on it.
        let mut units = vec![preamble("# header")];
        while map_bytes(&units) + 300 < exact {
            let id = units.len();
            units.push(decl_unit("pub fn f() {}", id, None));
        }
        let last = exact - map_bytes(&units) - 1;
        units.push(decl_unit(&"x".repeat(last), units.len(), None));
        assert_eq!(map_bytes(&units), exact, "the fixture missed the boundary");

        assert!(
            whole_payload(&units, &note).is_none(),
            "a map ending in a declaration costs a byte more than its parts"
        );

        let shorter = decl_unit(&"x".repeat(last - 1), units.len(), None);
        units.pop();
        units.push(shorter);
        let payload = whole_payload(&units, &note).expect("one byte under the budget must fit");
        assert_eq!(payload.len(), MAP_BUDGET, "the budget is there to be spent");

        units.push(separator());
        assert_eq!(map_bytes(&units), exact, "the separator restores the byte");
        assert!(
            whole_payload(&units, &note).is_some(),
            "a map already ending in a newline pays nothing for the marker's line"
        );
    }

    /// A declaration unit ends without a newline, so `cap_map` has to give the
    /// marker a line of its own rather than glue it to the last declaration.
    #[test]
    fn cap_map_handles_a_map_without_a_trailing_newline() {
        let mut units = vec![preamble("# header")];
        while map_bytes(&units) <= MAX_MAP_BYTES * 2 {
            let id = units.len();
            units.push(decl_unit("pub fn f() {}", id, None));
        }
        let full = map_bytes(&units);
        let out = cap_map(&units, &[], &HashSet::new(), "", full, Path::new("/tmp/x.rs")).payload;
        assert!(out.len() <= MAP_BUDGET, "{} bytes", out.len());
        assert!(out.starts_with("# header\n"));
        assert!(out.contains("map trimmed to"));
        let marker = out.lines().last().unwrap();
        assert!(marker.starts_with("# note:"), "marker glued on: {marker}");
    }

    /// The size a trimmed map reports is the size it delivers.
    ///
    /// That number is what a reader weighs against the full map's, so it has to
    /// count the bytes above the marker rather than the trim's own bookkeeping —
    /// charging every kept unit a newline overcounts by the one the last unit
    /// does not get. Sampled where the trim keeps most of a map, where the
    /// budget admits nothing but the header, and through `decide`, where a shed
    /// label sits in the same marker.
    #[test]
    fn a_trim_reports_the_bytes_it_delivers() {
        /// The bytes above the marker, and the number the marker claims for them.
        fn delivered_and_claimed(out: &str) -> (usize, usize) {
            let (map, marker) = match out.rfind("\n# note:") {
                Some(at) => (&out[..at], &out[at + 1..]),
                None => ("", out),
            };
            let claimed = marker
                .split("map trimmed to ")
                .nth(1)
                .and_then(|rest| rest.split(' ').next())
                .unwrap_or_else(|| panic!("no trim marker in: {marker}"))
                .parse()
                .unwrap();
            (map.len(), claimed)
        }

        let mut many = vec![preamble("# header")];
        while map_bytes(&many) <= MAX_MAP_BYTES * 2 {
            let id = many.len();
            many.push(decl_unit(&format!("pub fn f{id}() {{}}"), id, None));
        }
        // Nothing but the header can fit: every declaration is wider than the
        // whole budget, which is the path that keeps a single unit.
        let header_only = vec![
            preamble("# header"),
            decl_unit(&format!("pub fn wide({})", "a: u32, ".repeat(20_000)), 0, None),
        ];

        for (label, units) in [("most of a map", many), ("header only", header_only)] {
            let full = map_bytes(&units);
            let out = cap_map(&units, &[], &HashSet::new(), "", full, Path::new("/tmp/x.rs")).payload;
            let (delivered, claimed) = delivered_and_claimed(&out);
            assert_eq!(delivered, claimed, "{label}: marker in {out:.0}");
            assert!(out.len() <= MAP_BUDGET, "{label}: {} bytes", out.len());
        }

        let dir = TempDir::new().unwrap();
        let p = dir.path().join("bundle.rs");
        let doc = "/// ".to_string() + &"documented ".repeat(20);
        let src: String = (0..8000)
            .map(|i| format!("{doc}\npub fn f{i}() {{}}\n"))
            .collect();
        std::fs::write(&p, &src).unwrap();
        match decide(&ev("Read", Some(p), false), &opts(), replacing()) {
            Decision::Substitute { map } => {
                let (delivered, claimed) = delivered_and_claimed(&map);
                assert_eq!(delivered, claimed, "through decide");
            }
            other => panic!("expected a trimmed map: {other:?}"),
        }
    }

    /// One unreadably wide declaration must not take the rest of the map with it.
    ///
    /// Regression guard: the trim used to stop at the first line that did not fit,
    /// so a generated signature with thousands of parameters left a payload with
    /// no declarations at all — the failure the trim exists to prevent.
    #[test]
    fn a_single_enormous_declaration_does_not_hide_the_rest() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("wide.rs");
        let params: String = (0..9000).map(|i| format!("a{i}: u32, ")).collect();
        let mut src = format!("pub fn enormous({params}) {{}}\n");
        for i in 0..400 {
            src.push_str(&format!("pub fn api_{i}() {{}}\n"));
        }
        std::fs::write(&p, &src).unwrap();

        match decide(&ev("Read", Some(p), false), &opts(), replacing()) {
            Decision::Substitute { map } => {
                assert!(map.len() <= MAP_BUDGET, "{} bytes", map.len());
                for name in ["pub fn api_0", "pub fn api_399"] {
                    assert!(map.contains(name), "lost {name}:\n{}", &map[..300]);
                }
                assert!(map.contains("dropped"), "must say what it dropped");
            }
            other => panic!("expected a trimmed map: {other:?}"),
        }
    }

    /// A payload that both shed detail and got trimmed says so, and quotes the
    /// real file's map size rather than the already-shed one.
    #[test]
    fn a_trimmed_payload_still_names_what_was_shed() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("both.rs");
        let doc = "/// ".to_string() + &"documented ".repeat(20);
        let mut src = String::new();
        for i in 0..4000 {
            src.push_str(&format!("{doc}\nfn secret_helper_{i}() {{}}\n"));
            src.push_str(&format!("{doc}\npub fn api_{i}() {{}}\n"));
        }
        std::fs::write(&p, &src).unwrap();

        let full = crate::core::render_map(
            &crate::main_helpers::parse_file_for_hook(&p).unwrap(),
            &crate::core::MapOptions::default(),
        );
        match decide(&ev("Read", Some(p), false), &opts(), replacing()) {
            Decision::Substitute { map } => {
                assert!(map.len() <= MAP_BUDGET, "{} bytes", map.len());
                let note = map.lines().last().unwrap();
                assert!(note.contains("dropped, then"), "shed label missing: {note}");
                assert!(
                    note.contains(&full.len().to_string()),
                    "must quote the full map's size ({}): {note}",
                    full.len()
                );
            }
            other => panic!("expected a trimmed map: {other:?}"),
        }
    }

    /// A dropped declaration takes everything nested under it.
    ///
    /// A member kept without its type header reads as a member of the previous
    /// type — a map asserting a structure the file does not have, which is worse
    /// than a shorter one. The fixture puts the headers far above their members
    /// in width, which is the window where a header cannot fit and its members
    /// still can. Both funding orders are run over it: with no leaner render
    /// every rank is equal and the budget goes out in file order, and with one
    /// that carries the outer types alone a header is funded long after members
    /// that precede it, which is the order a real ladder produces. The
    /// invariants checked are the two claims the function makes — every
    /// declaration it kept still has all of its ancestors, and what it kept is
    /// written back in the file's order whatever order it was funded in.
    #[test]
    fn a_trim_never_keeps_a_unit_whose_parent_it_dropped() {
        let mut units = vec![preamble("# header")];
        let mut id = 0usize;
        while map_bytes(&units) <= MAP_BUDGET + 40_000 {
            let (outer, inner) = (id, id + 1);
            units.push(decl_unit(
                &format!("public class Outer{outer}{}  L1", "X".repeat(3000)),
                outer,
                None,
            ));
            units.push(decl_unit(
                &format!("    public static class Inner{outer}  L2"),
                inner,
                Some(outer),
            ));
            units.push(decl_unit(
                &format!("        void inner{outer}()  L3"),
                inner + 1,
                Some(inner),
            ));
            units.push(separator());
            units.push(decl_unit(
                &format!("    void survivor{outer}()  L4"),
                inner + 2,
                Some(outer),
            ));
            units.push(separator());
            id += 4;
        }
        // The leanest level of a ladder that keeps types and sheds their
        // members: it gives every header a rank its members do not have.
        let types_only: Vec<MapUnit> = units
            .iter()
            .filter(|u| u.parent.is_none() && matches!(u.kind, MapUnitKind::Declaration { .. }))
            .cloned()
            .collect();

        let parent_of: HashMap<usize, Option<usize>> = units
            .iter()
            .filter_map(|u| match u.kind {
                MapUnitKind::Declaration { id } => Some((id, u.parent)),
                _ => None,
            })
            .collect();
        let text_of = |id: usize| {
            units
                .iter()
                .find(|u| u.kind == MapUnitKind::Declaration { id })
                .map(|u| u.text.chars().take(40).collect::<String>())
                .unwrap_or_default()
        };

        let full = map_bytes(&units);
        let mut headers_kept: Vec<usize> = Vec::new();
        for (label, leaner) in [
            ("no leaner render", Vec::new()),
            ("types outrank members", vec![types_only.clone()]),
        ] {
            let Candidate {
                payload: out,
                delivered,
                ..
            } = cap_map(
                &units,
                &leaner,
                &HashSet::new(),
                "",
                full,
                Path::new("/tmp/wide.java"),
            );
            assert!(
                delivered > 0,
                "{label}: the trim delivered nothing to check"
            );
            assert!(out.len() <= MAP_BUDGET, "{label}: {} bytes", out.len());
            assert!(out.contains("dropped"), "{label}: no trim was triggered");

            let mut kept: HashSet<usize> = HashSet::new();
            let mut offsets: Vec<usize> = Vec::new();
            let mut members = 0usize;
            let mut headers = 0usize;
            for unit in &units {
                let MapUnitKind::Declaration { id } = unit.kind else {
                    continue;
                };
                let Some(at) = out.find(unit.text.as_str()) else {
                    continue;
                };
                kept.insert(id);
                offsets.push(at);
                if unit.parent.is_some() {
                    members += 1;
                } else {
                    headers += 1;
                }
            }
            assert!(
                members > 0,
                "{label}: no member survived, so nothing exercises the parent rule"
            );
            assert!(
                offsets.windows(2).all(|w| w[0] < w[1]),
                "{label}: the kept units are not in the file's order"
            );

            let mut orphans: Vec<String> = Vec::new();
            for &id in &kept {
                let mut ancestor = parent_of[&id];
                while let Some(a) = ancestor {
                    if !kept.contains(&a) {
                        orphans.push(format!("{:?} kept without {:?}", text_of(id), text_of(a)));
                    }
                    ancestor = parent_of[&a];
                }
            }
            assert!(
                orphans.is_empty(),
                "{label}: orphaned units: {:#?}",
                &orphans[..orphans.len().min(3)]
            );
            headers_kept.push(headers);
        }
        // Without this the second case could be funding in file order too, and
        // the run above would prove nothing the first one did not.
        assert!(
            headers_kept[1] > headers_kept[0],
            "ranking changed nothing: {} headers either way",
            headers_kept[0]
        );
    }

    /// Units nest by the declaration tree, not by how their text came out.
    ///
    /// Rendered text answers no question about depth: the separator a nested type
    /// emits sits between its parent's members rather than after the last one,
    /// and a doc comment keeps the indentation it had in the source, so its
    /// continuation lines land at columns like 3. Both used to file the member
    /// after them under the wrong type. Regression guard for the class: what
    /// nests under what is read off the tree, so no rendered line can lie
    /// about it.
    #[test]
    fn map_units_nest_by_declaration_rather_than_by_rendered_text() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("Outer.java");
        std::fs::write(
            &p,
            "public class Outer {\n\
             \x20   public static class Inner {\n\
             \x20       void innerMethod() {}\n\
             \x20   }\n\
             \n\
             \x20   /**\n\
             * Doc for afterInner.\n\
             */\n\
             \x20   void afterInner() {}\n\
             }\n",
        )
        .unwrap();
        let res = crate::main_helpers::parse_file_for_hook(&p).unwrap();
        let units = crate::core::render_map_units(&res, &crate::core::MapOptions::default());
        let unit_for = |needle: &str| {
            units
                .iter()
                .find(|u| u.text.contains(needle))
                .unwrap_or_else(|| panic!("no unit for {needle}:\n{}", join_units(&units)))
        };
        let id_for = |needle: &str| match unit_for(needle).kind {
            MapUnitKind::Declaration { id } => id,
            other => panic!("{needle} is {other:?}, not a declaration"),
        };

        assert_eq!(unit_for("class Inner").parent, Some(id_for("class Outer")));
        assert_eq!(unit_for("innerMethod").parent, Some(id_for("class Inner")));
        assert_eq!(
            unit_for("afterInner").parent,
            Some(id_for("class Outer")),
            "a member after a nested type belongs to the enclosing type"
        );
        assert!(
            units.iter().any(|u| u.kind == MapUnitKind::Separator),
            "the fixture must render the separator the bug rode in on"
        );
    }

    /// A payload never omits a declaration a whole candidate would have carried.
    ///
    /// The trim's budget is smaller than the fitting level's by the width of the
    /// marker it has to carry, so a trim of a richer level can run out inside the
    /// top rank. It then skips the one unit too wide for what is left and spends
    /// the remainder on cheaper declarations a rank below, which out-counts the
    /// whole candidate while missing part of the public surface that one carried.
    /// The fixture is built for that: the public surface is grown to the largest
    /// that still fits whole, its last member is wide enough to be the one
    /// skipped, and the private methods behind it are cheap enough to more than
    /// replace it by count.
    #[test]
    fn a_trimmed_candidate_never_omits_what_a_whole_one_carries() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("api.rs");
        let surface = widest_public_surface_that_fits(&p);

        write_public_surface(&p, surface);
        match decide(&ev("Read", Some(p.clone()), false), &opts(), replacing()) {
            Decision::Substitute { map } => {
                assert!(map.len() <= MAP_BUDGET, "{} bytes", map.len());
                assert_eq!(
                    map.matches("pub fn api_").count(),
                    surface,
                    "the payload dropped part of a public surface that fits whole"
                );
                assert!(
                    map.contains("pub fn wide"),
                    "the wide public declaration is exactly the one a trim skips"
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// No payload crosses the ceiling, on either side of the fit boundary.
    ///
    /// The two paths size themselves differently — the trim reserves the byte its
    /// marker sits on, a whole map is measured against the ceiling directly — so
    /// the boundary between them is where a byte goes missing. Sampled below it,
    /// on it, and above it, since a map measured *at* the ceiling is the one that
    /// gets delivered over it when the marker's own line is not counted.
    #[test]
    fn no_payload_crosses_the_ceiling_around_the_fit_boundary() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("api.rs");
        let surface = widest_public_surface_that_fits(&p);

        let (mut whole, mut trimmed) = (0usize, 0usize);
        for n in surface - 2..=surface + 2 {
            write_public_surface(&p, n);
            match decide(&ev("Read", Some(p.clone()), false), &opts(), replacing()) {
                Decision::Substitute { map } => {
                    assert!(
                        map.len() <= MAP_BUDGET,
                        "{n} public declarations delivered {} bytes",
                        map.len()
                    );
                    if map.contains("map trimmed to") {
                        trimmed += 1;
                    } else {
                        whole += 1;
                    }
                }
                other => panic!("{n}: unexpected: {other:?}"),
            }
        }
        assert!(
            whole > 0 && trimmed > 0,
            "the sweep stayed on one side of the boundary: {whole} whole, {trimmed} trimmed"
        );
    }

    /// A type of `n` cheap public methods, one wide public method, and private
    /// methods cheap enough that a trim can more than replace the wide one.
    fn write_public_surface(path: &std::path::Path, n: usize) {
        let params = (0..40)
            .map(|k| format!("a{k}: u32"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut body = String::new();
        for i in 0..n {
            body.push_str(&format!(
                "    pub fn api_{i:04}(&self, a: u32) -> u32 {{ a }}\n"
            ));
        }
        body.push_str(&format!("    pub fn wide(&self, {params}) {{}}\n"));
        for i in 0..2000 {
            body.push_str(&format!("    fn h_{i:04}(&self) {{}}\n"));
        }
        std::fs::write(path, format!("pub struct S;\n\nimpl S {{\n{body}}}\n")).unwrap();
    }

    /// The largest public surface [`write_public_surface`] can give the file with
    /// the shed level still fitting whole, which is the boundary both tests need.
    fn widest_public_surface_that_fits(path: &std::path::Path) -> usize {
        let fits = |n: usize| {
            write_public_surface(path, n);
            let res = crate::main_helpers::parse_file_for_hook(path).unwrap();
            let units = crate::core::render_map_units(&res, &map_levels()[2].1);
            with_note(join_units(&units), &shed_note(map_levels()[2].0, path)).len() <= MAP_BUDGET
        };
        let (mut small, mut large) = (1usize, 4000usize);
        while small + 1 < large {
            let mid = (small + large) / 2;
            if fits(mid) {
                small = mid;
            } else {
                large = mid;
            }
        }
        assert!(fits(small), "no public surface fits whole");
        assert!(!fits(small + 1), "{small} is not the boundary");
        small
    }

    /// A declaration keeps its number when the options drop its neighbours.
    ///
    /// [`survival_ranks`] matches two renders of one file by that number, so a
    /// number that shifted with the options would rank one declaration by
    /// another's survival. Two paths drop a declaration without rendering it, and
    /// each has to number what it skipped: the visibility filter, which skips a
    /// type and everything under it, and the member cap, which skips a member and
    /// everything under *that*. The cap is the one the leanest level of the
    /// ladder sets, so its numbering is what the top rank is read from. It is
    /// sampled below, at, and above the member count, since a cap equal to the
    /// count is the one that cuts nothing.
    #[test]
    fn a_declaration_keeps_its_number_across_renders() {
        use crate::core::MapOptions;
        let dir = TempDir::new().unwrap();
        let numbered = |path: &std::path::Path, opts: &MapOptions| -> HashMap<usize, String> {
            let res = crate::main_helpers::parse_file_for_hook(path).unwrap();
            crate::core::render_map_units(&res, opts)
                .iter()
                .filter_map(|u| match u.kind {
                    MapUnitKind::Declaration { id } => Some((id, u.text.clone())),
                    _ => None,
                })
                .collect()
        };
        let same_names =
            |part: &HashMap<usize, String>, whole: &HashMap<usize, String>, what: &str| {
                for (id, text) in part {
                    assert_eq!(
                        whole.get(id),
                        Some(text),
                        "{what}: declaration {id} is {text:?} in one render and {:?} in the other",
                        whole.get(id)
                    );
                }
            };

        let hidden = dir.path().join("hidden.rs");
        std::fs::write(
            &hidden,
            "struct Hidden {\n    a: u32,\n    b: u32,\n}\n\npub fn api(x: u32) -> u32 { x }\n",
        )
        .unwrap();
        let full = numbered(&hidden, &MapOptions::default());
        let lean = numbered(
            &hidden,
            &MapOptions {
                include_private: false,
                include_fields: false,
                ..MapOptions::default()
            },
        );
        assert!(
            full.len() > lean.len() + 1,
            "the fixture must hide a type and its members: {} of {}",
            lean.len(),
            full.len()
        );
        same_names(&lean, &full, "visibility filter");

        // A type of six members, with a declaration after it whose number is the
        // one the skip has to get right. No adapter nests a declaration inside a
        // member, so the subtree half of that arithmetic is defensive and only
        // the cut member itself can be pinned here.
        let widget = dir.path().join("widget.py");
        let mut src = String::from("class Widget:\n");
        for i in 0..6 {
            src.push_str(&format!("    def m{i}(self):\n        pass\n"));
        }
        src.push_str("\ndef api():\n    pass\n");
        std::fs::write(&widget, &src).unwrap();
        let full = numbered(&widget, &MapOptions::default());
        assert_eq!(
            full.len(),
            8,
            "the fixture must be one type of six members plus a function: {full:?}"
        );
        let mut cut = 0usize;
        for cap in [2usize, 6, 7] {
            let capped = numbered(
                &widget,
                &MapOptions {
                    max_members: Some(cap),
                    ..MapOptions::default()
                },
            );
            if capped.len() < full.len() {
                cut += 1;
            }
            same_names(&capped, &full, &format!("member cap {cap}"));
        }
        assert_eq!(cut, 1, "only a cap below the member count may cut");
    }

    /// A trim spends the budget on the public surface before the rest.
    ///
    /// Filling in file order handed over 1733 private helpers and not one of a
    /// file's 500 public functions, because the publics run to the end of a file
    /// twice the budget. The shed level that keeps exactly those publics is
    /// 19 KB, so that payload was worse than a candidate it outranked on count
    /// alone. Interleaving the two in the fixture is deliberate: with the publics
    /// in one block at the front, file order would keep them by luck.
    #[test]
    fn a_trim_keeps_the_public_surface_before_it_spends_the_rest() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("interleaved.rs");
        let mut src = String::new();
        for i in 0..500 {
            src.push_str(&format!("pub fn api_{i:03}(a: u32) -> u32 {{ a }}\n"));
            for j in 0..6 {
                src.push_str(&format!("fn helper_{i:03}_{j}(a: u32) -> u32 {{ a }}\n"));
            }
        }
        std::fs::write(&p, &src).unwrap();

        match decide(&ev("Read", Some(p), false), &opts(), replacing()) {
            Decision::Substitute { map } => {
                assert!(map.len() <= MAP_BUDGET, "{} bytes", map.len());
                assert_eq!(
                    map.matches("pub fn api_").count(),
                    500,
                    "the public surface fits in a third of the budget and must survive whole"
                );
                let helpers = map.matches("fn helper_").count();
                assert!(
                    helpers > 500,
                    "only {helpers} private helpers survived; the rest of the budget is theirs"
                );
            }
            other => panic!("expected a trimmed map: {other:?}"),
        }
    }

    /// Shedding is chosen for what it delivers, not for fitting.
    ///
    /// A file that is mostly private fits the moment private items go, and that
    /// "success" hands over only the public few while most of the budget goes
    /// unspent — where trimming the level before it carries an order of
    /// magnitude more. Regression guard: this file used to deliver 50
    /// declarations, and an all-private one delivered none at all.
    #[test]
    fn a_mostly_private_file_keeps_private_declarations_by_trimming() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("mostly_private.rs");
        let mut src = String::new();
        for i in 0..2000 {
            src.push_str(&format!("fn priv_{i:04}(a: u32) -> u32 {{ a }}\n"));
        }
        for i in 0..50 {
            src.push_str(&format!("pub fn api_{i:03}(a: u32) -> u32 {{ a }}\n"));
        }
        std::fs::write(&p, &src).unwrap();

        match decide(&ev("Read", Some(p), false), &opts(), replacing()) {
            Decision::Substitute { map } => {
                assert!(map.len() <= MAP_BUDGET, "{} bytes", map.len());
                let private_kept = map.matches("fn priv_").count();
                assert!(
                    private_kept > 500,
                    "only {private_kept} private declarations survived; the shed level \
                     would have delivered none of them"
                );
                // The budget is there to be spent: a payload that fits with room
                // to spare has thrown away declarations it could have carried.
                assert!(
                    map.len() > MAP_BUDGET / 2,
                    "payload is {} bytes of a {MAP_BUDGET} budget",
                    map.len()
                );
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// A file with no declarations at all still gets a readable payload: the note
    /// goes on its own line rather than gluing itself to the header.
    #[test]
    fn a_note_never_glues_itself_to_the_header_line() {
        let header = "# /tmp/x.rs ([medium], 3000 lines, 1 chars, 3000 methods)".to_string();
        let note = shed_note("doc comments", Path::new("/tmp/x.rs"));
        let out = with_note(header.clone(), &note);
        assert!(out.starts_with(&format!("{header}\n")), "{out}");
        assert!(out.ends_with(&note), "{out}");
    }

    #[test]
    fn always_flag_substitutes_below_threshold() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("small.rs");
        std::fs::write(&p, "fn main() {}\n").unwrap();
        let d = decide(
            &ev("Read", Some(p), false),
            &DecideOpts {
                min_lines: 200,
                always: true,
            },
            replacing(),
        );
        assert!(matches!(d, Decision::Substitute { .. }));
    }
}
