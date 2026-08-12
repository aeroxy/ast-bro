use std::path::Path;

use super::event::{Channel, Decision, ToolCallEvent};

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

/// Ceiling on what the hook delivers, in bytes, notice included.
///
/// The host refuses a read at roughly 25k tokens, so answering with a payload of
/// the same order defeats the point. 64 KB is about 16k tokens: under the limit
/// the refusal was about, and above every real map measured — Apache Calcite's
/// `SqlFunctions.java`, 7771 hand-written lines and the largest map in that
/// repository, fits once doc comments and attributes are shed. What does not fit
/// is a minified bundle, whose size is declaration count rather than detail, and
/// that is what [`cap_map`] is for.
///
/// The ceiling is hard: [`MAP_BUDGET`] holds back room for the channel notice and
/// the marker, both of which are built before anything is cut.
const MAX_MAP_BYTES: usize = 64 * 1024;

/// What a map may occupy once the channel notice is accounted for.
///
/// `io::payload` prepends a notice to every map, so a map sized against
/// [`MAX_MAP_BYTES`] alone would be delivered above it.
const MAP_BUDGET: usize = MAX_MAP_BYTES - super::io::MAX_NOTICE_BYTES;

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
    let mut full_bytes = 0usize;
    // The candidate that delivers the most entries, not the first that fits.
    // Shedding is lossy in a different currency from trimming: a file of 2900
    // private and 100 public functions fits once private items go, and that
    // "success" hands over 100 declarations while leaving 93% of the budget
    // unused, where trimming the level before it carries about 1350.
    let mut best: Option<(usize, String)> = None;
    for (shed, opts) in map_levels() {
        let units = crate::core::render_map_units(&res, &opts);
        let map = units.join("\n");
        if shed.is_empty() {
            full_bytes = map.len();
        }
        // Naming what is missing is the whole point: absence of a private helper
        // from this map must not read as absence from the file. The note is built
        // before the fit decision so it counts against the budget.
        let note = if shed.is_empty() {
            String::new()
        } else {
            shed_note(shed, path)
        };
        let fits = map.len() + note.len() <= MAP_BUDGET;
        let (payload, delivered) = if fits {
            (with_note(map, &note), entry_count(&units))
        } else {
            cap_map(units, shed, full_bytes, path)
        };
        // Strictly greater, so the richest level wins a tie.
        if best.as_ref().is_none_or(|(most, _)| delivered > *most) {
            best = Some((delivered, payload));
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
    best.map(|(_, payload)| payload)
}

/// Entries a map carries: everything but its separators.
///
/// Used only to compare one candidate against another, and it does not count
/// quite the same things at every level — `... +N more member(s)` markers exist
/// only where `max_members` is set, which is the last level alone, and they are
/// counted there. That cannot change a choice. Against the level before it, a
/// capped type contributes `1 + min(50, N) + 1` where that one contributes
/// `1 + N` with `N >= 51`, so the capped count is never higher, and the one tie a
/// marker can manufacture, at `N == 51`, goes to the earlier level on the strict
/// comparison. To beat a *trimmed* earlier level it would have to fit whole while
/// that one overflowed, which bounds it by the same budget the trim saturates.
fn entry_count(units: &[String]) -> usize {
    units.iter().filter(|u| unit_indent(u).is_some()).count()
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

/// The column a unit sits at, or `None` for a separator.
///
/// A unit's own indentation is the render prefix, so it is a reliable depth
/// signal where a physical line is not: a doc comment keeps the indentation it
/// had in the source, so its continuation lines land at arbitrary columns. A
/// separator carries no indentation and therefore no structure — treating its
/// column as zero is what let members outlive the header they belong to.
fn unit_indent(unit: &str) -> Option<usize> {
    let first = unit.lines().next().unwrap_or("");
    if first.trim().is_empty() {
        return None;
    }
    Some(first.len() - first.trim_start_matches(' ').len())
}

/// Drops whole units from a map until it fits [`MAP_BUDGET`], saying what went.
///
/// The last resort, reached only when shedding every level of detail still left
/// the map too big — a minified bundle, where the size is thousands of
/// declarations rather than detail about a few. A silently short map is worse
/// than a trimmed one: the reader treats a map as the file's whole shape and
/// concludes a symbol does not exist, so the marker names what was shed on the
/// way here, how many entries went, and the file's real size.
///
/// Two rules keep the result truthful. An over-wide unit is skipped rather than
/// ending the scan, because one generated signature with thousands of parameters
/// would otherwise drop every declaration after it. And a dropped unit takes
/// every following unit indented deeper than it, because indentation is the only
/// parent-child signal a map carries and a member kept without its type header
/// reads as a member of the previous type.
fn cap_map(units: Vec<String>, shed: &str, full_bytes: usize, path: &Path) -> (String, usize) {
    let total = units.iter().map(String::len).sum::<usize>() + units.len();
    // Size the marker against the widest numbers it can carry, so the budget it
    // comes out of is never too small; the real numbers are no wider, since what
    // is kept never exceeds the whole map and what is dropped never exceeds the
    // unit count. One byte is held back for the newline before the marker.
    let budget = MAP_BUDGET
        .saturating_sub(trim_note(total, full_bytes, units.len(), shed, path).len())
        .saturating_sub(1);
    let mut kept: Vec<String> = Vec::new();
    let mut len = 0usize;
    let mut dropped = 0usize;
    let mut skipped_parent: Option<usize> = None;
    for unit in units {
        let indent = unit_indent(&unit);
        if let (Some(level), Some(column)) = (skipped_parent, indent) {
            if column > level {
                dropped += 1;
                continue;
            }
            skipped_parent = None;
        }
        let cost = unit.len() + usize::from(!kept.is_empty());
        if len + cost <= budget {
            len += cost;
            kept.push(unit);
        } else {
            // A separator carries no structure, so dropping one starts no
            // subtree; leaving the state alone keeps the enclosing one in force.
            if let Some(column) = indent {
                dropped += 1;
                skipped_parent = Some(column);
            }
        }
    }
    let note = trim_note(len, full_bytes, dropped, shed, path);
    let delivered = kept.iter().filter(|u| unit_indent(u).is_some()).count();
    let out = with_note(kept.join("\n"), &note);
    debug_assert!(out.len() <= MAP_BUDGET, "trim overshot the budget");
    (out, delivered)
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

    /// `cap_map` cuts on a line boundary even when the last line has no newline,
    /// which is what a file without a trailing newline produces.
    #[test]
    fn cap_map_handles_a_map_without_a_trailing_newline() {
        let line = "pub fn f() {}\n";
        let mut map = String::from("# header\n");
        while map.len() <= MAX_MAP_BYTES * 2 {
            map.push_str(line);
        }
        map.pop(); // the last unit now ends without a newline
        let len = map.len();
        let units: Vec<String> = map.split('\n').map(str::to_string).collect();
        let (out, _) = cap_map(units, "", len, Path::new("/tmp/x.rs"));
        assert!(out.len() <= MAP_BUDGET, "{} bytes", out.len());
        assert!(out.starts_with("# header\n"));
        assert!(out.contains("map trimmed to"));
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

    /// A dropped unit takes its children with it, whatever the text looks like.
    ///
    /// Indentation is the only parent-child signal a map has, so a member kept
    /// without its type header reads as a member of the previous type — a map
    /// asserting a structure the file does not have, which is worse than a
    /// shorter one. Two shapes in the fixture are the ones that broke it before:
    /// a nested type emits a separator *inside* its parent's member list, and a
    /// doc comment keeps its source indentation, so its continuation lines land
    /// at columns like 3 that say nothing about depth. The invariant checked is
    /// exactly the claim: every unit the trim kept still has all of its
    /// ancestors.
    #[test]
    fn a_trim_never_keeps_a_unit_whose_parent_it_dropped() {
        let joined_len = |u: &[String]| u.iter().map(String::len).sum::<usize>() + u.len();
        // Headers far wider than their members, so the budget runs out in the
        // window where a header cannot fit but its members still can.
        let mut units: Vec<String> = vec!["# header".into()];
        let mut n = 2;
        while joined_len(&units) <= MAP_BUDGET + 40_000 {
            units.push(format!("public class Outer{n}{}  L{n}", "X".repeat(3000)));
            units.push(format!("    public static class Inner{n}  L{}", n + 1));
            units.push(format!("        void inner{n}()  L{}", n + 2));
            // The separator a nested type emits, sitting between its parent's
            // members rather than after the last one.
            units.push(String::new());
            units.push(format!("    void survivor{n}()  L{}", n + 3));
            // A Javadoc unit: prefixed first line, continuation lines carrying
            // the indentation they had in the source.
            units.push(format!(
                "    /**\n   * Doc for docd{n}.\n   */\n    void docd{n}()  L{}",
                n + 4
            ));
            units.push(String::new());
            n += 8;
        }

        // Each unit's ancestors, by the nearest preceding unit of smaller
        // indent. Computed here rather than through `unit_indent`, so the
        // expectation does not depend on the function under test: a blank unit
        // is not a node, and reading its column as zero is the very mistake
        // being guarded against.
        let mut ancestors: Vec<(String, Vec<String>)> = Vec::new();
        let mut stack: Vec<(usize, String)> = Vec::new();
        for unit in &units {
            if unit.trim().is_empty() {
                continue;
            }
            let first = unit.lines().next().unwrap_or("");
            let column = first.len() - first.trim_start_matches(' ').len();
            while stack.last().is_some_and(|(c, _)| *c >= column) {
                stack.pop();
            }
            ancestors.push((
                unit.clone(),
                stack.iter().map(|(_, u)| u.clone()).collect(),
            ));
            stack.push((column, unit.clone()));
        }

        let full = joined_len(&units);
        let (out, delivered) = cap_map(units, "", full, Path::new("/tmp/wide.java"));
        assert!(delivered > 0, "the trim must deliver something to check");
        assert!(out.len() <= MAP_BUDGET, "{} bytes", out.len());
        assert!(out.contains("dropped"), "the fixture must trigger a trim");

        let mut checked = 0usize;
        let mut orphans: Vec<String> = Vec::new();
        for (unit, parents) in &ancestors {
            if !out.contains(unit.as_str()) {
                continue;
            }
            checked += 1;
            for parent in parents {
                if !out.contains(parent.as_str()) {
                    orphans.push(format!(
                        "{:?} kept without {:?}",
                        unit.lines().last().unwrap_or(unit),
                        parent.chars().take(40).collect::<String>()
                    ));
                }
            }
        }
        assert!(checked > 0, "the fixture kept nothing to check");
        assert!(orphans.is_empty(), "orphaned units: {:#?}", &orphans[..orphans.len().min(3)]);
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
