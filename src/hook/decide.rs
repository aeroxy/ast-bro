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
    // The smallest map any level produced, for the case where none of them fit
    // and the marker has to say both what was shed and what was cut. Smallest
    // rather than last: the `max_members` level trades members for `+N more`
    // markers, which can leave it larger than the level before it.
    let mut smallest: Option<(&'static str, String)> = None;
    for (shed, opts) in map_levels() {
        let map = crate::core::render_map(&res, &opts);
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
        if map.len() + note.len() <= MAP_BUDGET {
            return Some(map + &note);
        }
        if smallest.as_ref().is_none_or(|(_, best)| map.len() < best.len()) {
            smallest = Some((shed, map));
        }
    }
    let (shed, map) = smallest.expect("map_levels is never empty");
    Some(cap_map(map, shed, full_bytes, path))
}

/// The line a shed map ends with, naming what is gone and how to get it.
fn shed_note(shed: &str, path: &Path) -> String {
    format!(
        "# note: {shed} dropped to fit the {MAX_MAP_BYTES} byte cap; `ast-bro map {}` for the rest\n",
        path.display()
    )
}

/// Drops whole lines from `map` until it fits [`MAP_BUDGET`], saying what went.
///
/// The last resort, reached only when shedding every level of detail still left
/// the map too big — a minified bundle, where the size is thousands of
/// declarations rather than detail about a few. A silently short map is worse
/// than a trimmed one: the reader treats a map as the file's whole shape and
/// concludes a symbol does not exist, so the marker names what was shed on the
/// way here, how many lines went, and the file's real size.
///
/// Over-long lines are skipped rather than ending the scan. One declaration can
/// be arbitrarily wide — a generated signature with thousands of parameters — and
/// stopping at it would drop every declaration after it, which is the failure
/// this function exists to prevent.
fn cap_map(map: String, shed: &str, full_bytes: usize, path: &Path) -> String {
    let total_lines = map.split_inclusive('\n').count();
    // Size the marker against the widest numbers it can carry, so the budget it
    // comes out of is never too small; the real numbers are no wider, since
    // `kept` never exceeds `map.len()` and `dropped` never exceeds `total_lines`.
    let budget = MAP_BUDGET
        .saturating_sub(trim_note(map.len(), full_bytes, total_lines, shed, path).len());
    let mut kept = String::with_capacity(budget.min(map.len()));
    let mut dropped = 0usize;
    // Indentation is the only parent-child signal a map carries, so a skipped
    // line takes its children with it. Keeping a member whose type header did
    // not fit would file it under the previous type — a map that asserts a
    // structure the file does not have, which is worse than a shorter one.
    let mut skipped_parent: Option<usize> = None;
    for line in map.split_inclusive('\n') {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if let Some(level) = skipped_parent {
            if indent > level {
                dropped += 1;
                continue;
            }
            skipped_parent = None;
        }
        if kept.len() + line.len() <= budget {
            kept.push_str(line);
        } else {
            dropped += 1;
            skipped_parent = Some(indent);
        }
    }
    let note = trim_note(kept.len(), full_bytes, dropped, shed, path);
    debug_assert!(
        kept.len() + note.len() <= MAP_BUDGET,
        "trim overshot the budget"
    );
    kept + &note
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
            format!(", {dropped} line(s) dropped")
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
        map.pop(); // last line now ends the string with no newline
        let len = map.len();
        let out = cap_map(map, "", len, Path::new("/tmp/x.rs"));
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
                assert!(map.contains("line(s) dropped"), "must say what it dropped");
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

    /// A skipped line takes its children with it.
    ///
    /// Indentation is the only parent-child signal a map has, so a member kept
    /// without its type header would be filed under the previous type — a map
    /// asserting a structure the file does not have, which is worse than a
    /// shorter one. The invariant checked is exactly that: every line the trim
    /// kept still has each of its ancestors.
    #[test]
    fn a_trim_never_keeps_a_line_whose_parent_it_dropped() {
        // Headers far wider than their members, so the budget runs out in the
        // window where a header cannot fit but its members still can.
        let mut map = String::from("# header\n");
        let mut n = 2;
        while map.len() <= MAP_BUDGET + 40_000 {
            map.push_str(&format!("pub struct Wide{n}{}  L{n}\n", "X".repeat(2000)));
            map.push_str(&format!("    pub fn m(&self)  L{}\n", n + 1));
            map.push_str(&format!("    pub fn n(&self)  L{}\n", n + 2));
            map.push('\n');
            n += 4;
        }

        let indent_of = |l: &str| l.len() - l.trim_start_matches(' ').len();
        // Each non-blank line's ancestors, by the nearest preceding line of
        // smaller indent. Every line in the fixture is unique, so membership in
        // the output is enough to tell kept from dropped.
        let mut ancestors: Vec<(String, Vec<String>)> = Vec::new();
        let mut stack: Vec<(usize, String)> = Vec::new();
        for line in map.lines().filter(|l| !l.trim().is_empty()) {
            let ind = indent_of(line);
            while stack.last().is_some_and(|(i, _)| *i >= ind) {
                stack.pop();
            }
            ancestors.push((
                line.to_string(),
                stack.iter().map(|(_, l)| l.clone()).collect(),
            ));
            stack.push((ind, line.to_string()));
        }

        let full = map.len();
        let out = cap_map(map, "", full, Path::new("/tmp/wide.rs"));
        assert!(out.len() <= MAP_BUDGET, "{} bytes", out.len());
        assert!(out.contains("line(s) dropped"), "the fixture must trigger a trim");

        let kept: std::collections::HashSet<&str> =
            out.lines().map(|l| l.trim_end()).collect();
        let mut checked = 0usize;
        for (line, parents) in &ancestors {
            if !kept.contains(line.trim_end()) {
                continue;
            }
            for parent in parents {
                assert!(
                    kept.contains(parent.trim_end()),
                    "kept {line:?} but dropped its parent {parent:?}"
                );
            }
            checked += 1;
        }
        assert!(checked > 0, "the fixture kept nothing to check");
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
