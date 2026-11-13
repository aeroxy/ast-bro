//! Doc-comment coverage for the Go adapter against
//! `tests/fixtures/go_adapter/docs.go`. Asserts:
//!   1. A grouped declaration's comment documents the group, not whichever
//!      member happens to come first (issue #46). It travels as the
//!      member's `group`, is marked `[group]` in text, and prints once per
//!      block — including when the block above carries the same comment
//!      word for word.
//!   2. A member of a group keeps its own comment; the group's comment
//!      never displaces it.
//!   3. Struct fields and interface methods report their doc comments
//!      (issue #47), so an empty `docs` there means the declaration
//!      carries no comment rather than that none was read.
//!   4. A declaration's doc comment is the comment group `go/parser` gives
//!      it: the group ending on the line directly above, without the
//!      comments that continue the line of the token before it. The cases
//!      where several comments share a line are in
//!      `tests/fixtures/go_comment_runs/runs.go`.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

const FIXTURE: &str = "tests/fixtures/go_adapter/docs.go";

fn run(args: &[&str]) -> String {
    let out = Command::new(bin())
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("run");
    assert!(out.status.success(), "exit non-zero: {:?}", out);
    String::from_utf8(out.stdout).expect("utf8")
}

fn json() -> serde_json::Value {
    json_of(FIXTURE)
}

fn json_of(path: &str) -> serde_json::Value {
    serde_json::from_str(&run(&["map", path, "--json"])).expect("json")
}

/// Every declaration in the file, flattened, keyed by name. Where two
/// declarations share a name, the first in source order wins: the fixture
/// gives the field `Square` the name of its own struct, and `Board` a field
/// named `Red`, and each test here means the grouped declaration.
fn decls(v: &serde_json::Value) -> std::collections::HashMap<String, serde_json::Value> {
    fn walk(
        list: &[serde_json::Value],
        out: &mut std::collections::HashMap<String, serde_json::Value>,
    ) {
        for d in list {
            out.entry(d["name"].as_str().unwrap_or_default().to_string())
                .or_insert_with(|| d.clone());
            if let Some(children) = d["children"].as_array() {
                walk(children, out);
            }
        }
    }
    let mut out = std::collections::HashMap::new();
    walk(v["files"][0]["declarations"].as_array().unwrap(), &mut out);
    out
}

/// A declaration's own doc comment (`field` = "docs"), or its group's
/// (`field` = "group").
fn docs(d: &serde_json::Value, field: &str) -> Vec<String> {
    let v = if field == "group" {
        &d["group"]["docs"]
    } else {
        &d[field]
    };
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|x| x.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn group_comment_is_not_any_members_doc_comment() {
    let v = json();
    let d = decls(&v);

    for member in ["Red", "Blue", "MaxBytes", "MaxHops", "Square"] {
        assert!(
            !docs(&d[member], "docs")
                .iter()
                .any(|x| x.contains("palette")
                    || x.contains("bound every request")
                    || x.contains("shapes the renderer")),
            "{member} claimed its group's comment as its own doc comment:\n{:#?}",
            d[member]["docs"]
        );
    }

    // The comment is reported, just not as a member's own.
    assert_eq!(
        docs(&d["Red"], "group"),
        docs(&d["Blue"], "group"),
        "both members of a group carry the same group docs"
    );
    assert!(
        docs(&d["Blue"], "group")[0].contains("palette"),
        "const group's comment missing from the group:\n{:#?}",
        d["Blue"]
    );
    assert!(
        docs(&d["MaxHops"], "group")[0].contains("bound every request"),
        "var group's comment missing from the group:\n{:#?}",
        d["MaxHops"]
    );
    assert!(
        docs(&d["Square"], "group")[0].contains("shapes the renderer"),
        "type group's comment missing from the group:\n{:#?}",
        d["Square"]
    );
}

#[test]
fn ungrouped_declaration_still_gets_its_comment() {
    let v = json();
    let d = decls(&v);
    assert!(
        docs(&d["Standalone"], "docs")[0].contains("not in a group"),
        "ungrouped const lost its doc comment:\n{:#?}",
        d["Standalone"]
    );
    assert!(
        docs(&d["Standalone"], "group").is_empty(),
        "ungrouped const must not report a group:\n{:#?}",
        d["Standalone"]
    );
}

#[test]
fn a_groups_member_keeps_its_own_comment() {
    let v = json();
    let d = decls(&v);
    assert!(
        docs(&d["Red"], "docs")[0].contains("Red is the default"),
        "first member of a group lost its own doc comment:\n{:#?}",
        d["Red"]
    );
    assert!(
        docs(&d["Circle"], "docs")[0].contains("Circle is round"),
        "first type of a type group lost its own doc comment:\n{:#?}",
        d["Circle"]
    );
}

#[test]
fn struct_fields_and_interface_methods_report_docs() {
    let v = json();
    let d = decls(&v);
    assert!(
        docs(&d["Path"], "docs")[0].contains("request target"),
        "struct field lost its doc comment:\n{:#?}",
        d["Path"]
    );
    assert!(
        docs(&d["Radius"], "docs")[0].contains("unit in metres"),
        "field of a grouped type lost its doc comment:\n{:#?}",
        d["Radius"]
    );
    assert!(
        docs(&d["Do"], "docs")[0].contains("sends one request"),
        "interface method lost its doc comment:\n{:#?}",
        d["Do"]
    );
    // An undocumented member stays undocumented — the fix must not invent
    // documentation for one out of the comment above its neighbour.
    assert!(
        docs(&d["Close"], "docs").is_empty(),
        "undocumented interface method gained a doc comment:\n{:#?}",
        d["Close"]
    );
}

#[test]
fn a_trailing_comment_stays_with_the_line_it_is_on() {
    let v = json();
    let d = decls(&v);
    for (name, neighbour) in [
        ("Hops", "measured in bytes"),
        ("Receive", "trailing comment"),
    ] {
        assert!(
            !docs(&d[name], "docs").iter().any(|x| x.contains(neighbour)),
            "{name} inherited the trailing comment of the declaration above it:\n{:#?}",
            d[name]["docs"]
        );
    }
    assert!(
        docs(&d["Size"], "docs").is_empty(),
        "a field's own trailing comment is not a doc comment:\n{:#?}",
        d["Size"]
    );
}

/// The own `docs` of each named declaration in
/// `tests/fixtures/go_comment_runs/runs.go`, for one `assert_eq!` that
/// prints every mismatch at once.
fn run_docs(names: &[&str]) -> std::collections::BTreeMap<String, Vec<String>> {
    let d = decls(&json_of("tests/fixtures/go_comment_runs/runs.go"));
    names
        .iter()
        .map(|n| (n.to_string(), docs(&d[*n], "docs")))
        .collect()
}

fn expected(rows: &[(&str, &[&str])]) -> std::collections::BTreeMap<String, Vec<String>> {
    rows.iter()
        .map(|(n, docs)| (n.to_string(), docs.iter().map(|s| s.to_string()).collect()))
        .collect()
}

#[test]
fn comments_sharing_a_line_join_one_doc_comment() {
    let rows: &[(&str, &[&str])] = &[
        ("SameLine", &["/* block */", "// and line"]),
        ("ThreeOnTwoLines", &["// one", "/* two */", "/* three */"]),
        ("AfterMultiline", &["/* first\n   spans */", "// then"]),
        ("TwoOnOneLine", &["/* c */", "/* d */"]),
        ("Lead", &["/* lead */"]),
    ];
    let names: Vec<&str> = rows.iter().map(|(n, _)| *n).collect();
    assert_eq!(run_docs(&names), expected(rows));
}

#[test]
fn a_comment_group_ending_on_the_declarations_own_line_is_not_its_doc() {
    let rows: &[(&str, &[&str])] = &[("Inline", &[]), ("Lead", &["/* lead */"])];
    let names: Vec<&str> = rows.iter().map(|(n, _)| *n).collect();
    assert_eq!(run_docs(&names), expected(rows));
}

#[test]
fn comments_continuing_the_previous_tokens_line_document_nothing_below() {
    let rows: &[(&str, &[&str])] = &[
        ("AfterBrace", &[]),
        ("AfterAnnotated", &[]),
        ("AfterParen", &[]),
        ("BelowTrailed", &["// y"]),
        ("V", &[]),
        ("B", &[]),
        ("AfterKeyword", &[]),
    ];
    let names: Vec<&str> = rows.iter().map(|(n, _)| *n).collect();
    assert_eq!(run_docs(&names), expected(rows));
}

#[test]
fn a_comment_spanning_lines_after_a_declaration_ends_its_line_comment() {
    let rows: &[(&str, &[&str])] = &[
        ("Y", &["// c"]),
        ("W", &["/* z2 */"]),
        ("BelowTrailed", &["// y"]),
    ];
    let names: Vec<&str> = rows.iter().map(|(n, _)| *n).collect();
    assert_eq!(run_docs(&names), expected(rows));
}

#[test]
fn group_docs_render_once_and_marked() {
    let s = run(&["map", FIXTURE]);
    assert_eq!(
        s.matches("[group] // Colors are the palette").count(),
        1,
        "a group's comment renders once, not once per member:\n{s}"
    );
    assert!(
        s.contains("[group] // Limits bound every request"),
        "var group's comment missing from map output:\n{s}"
    );
    // Unmarked, the line directly above `const Red` reads as Red's own.
    assert!(
        !s.contains("\n    // Colors are the palette"),
        "group comment rendered as if it were a member's own:\n{s}"
    );
}

#[test]
fn group_docs_go_above_the_first_member_that_renders() {
    for (args, first_rendered) in [
        (&[][..], "const quantum = 10"),
        (&["--no-private"][..], "const Slots = 4"),
    ] {
        let s = run(&[&["map", FIXTURE][..], args].concat());
        assert_eq!(
            s.matches("[group] // Tuning holds").count(),
            1,
            "map {args:?}:\n{s}"
        );
        assert!(
            s.contains(&format!(
                "[group] // Tuning holds the scheduler's knobs.\n    {first_rendered}"
            )),
            "map {args:?} must print the group's comment directly above {first_rendered:?}:\n{s}"
        );
    }
}

/// Every member of a documented block stays visually attached to the
/// `[group]` line documenting it. The three block kinds are the axis: a
/// `const` / `var` group renders its members as consecutive one-liners,
/// while a `type` group's members each push a trailing blank line, which
/// is the case that separated a later member from its marker.
#[test]
fn a_groups_members_are_not_split_from_its_marker() {
    let s = run(&["map", FIXTURE]);
    for (marker, last_member) in [
        ("[group] // Colors are the palette", "const Blue"),
        ("[group] // Limits bound every request", "var MaxHops"),
        ("[group] // Shapes are the shapes", "type Square"),
    ] {
        let from = s
            .find(marker)
            .unwrap_or_else(|| panic!("no {marker:?}:\n{s}"));
        let to = s[from..]
            .find(last_member)
            .unwrap_or_else(|| panic!("no {last_member:?} after {marker:?}:\n{s}"));
        // A separator renders as an empty line, so a blank line inside the
        // region is exactly "\n\n". Scanning `lines()`
        // instead would count the indent before `last_member` as blank.
        assert!(
            !s[from..from + to].contains("\n\n"),
            "a blank line splits {last_member:?} from {marker:?}:\n{s}"
        );
    }
    // The blank line still separates the block from what follows it.
    assert!(
        s.contains("type Square struct  L46\n        Square int  L46\n\n"),
        "the group's own trailing blank line was swallowed:\n{s}"
    );

    // With the markers hidden there is nothing for a closed gap to attach
    // a member to, so a documented block has to render exactly like an
    // undocumented one. Otherwise the spacing alone tells the reader which
    // block carries the comment they asked to hide.
    let hidden = run(&["map", FIXTURE, "--no-docs"]);
    // `Circle`/`Square` is the pair under test: it is the documented block,
    // so it is the one whose spacing the gate can get wrong.
    // `Undocumented`/`AlsoNone` carries no group and separates in every
    // mode — it is the reference the first pair has to match, not a second
    // test of the gate.
    for (first, second) in [
        ("type Circle struct", "type Square"),
        ("type Undocumented struct", "type AlsoNone"),
    ] {
        let from = hidden
            .find(first)
            .unwrap_or_else(|| panic!("no {first:?}:\n{hidden}"));
        let to = hidden[from..]
            .find(second)
            .unwrap_or_else(|| panic!("no {second:?} after {first:?}:\n{hidden}"));
        assert!(
            hidden[from..from + to].contains("\n\n"),
            "--no-docs must separate {second:?} from {first:?} the way an \
             undocumented block is separated:\n{hidden}"
        );
    }
}

/// `surface` resolves a package's public API from its own `SurfaceEntry`,
/// so a resolver that copies `docs` without `group` reports every member
/// of a documented block as undocumented — the misreading this change
/// exists to prevent, one surface over.
#[test]
fn surface_carries_a_blocks_documentation() {
    // The expected set comes from the fixture, not from a list beside it,
    // so a member the fixture gains is covered without an edit here.
    //
    // Both sides are keyed by kind, name, and line, and the surface side is
    // narrowed to this file by its whole path. The fixture holds a symbol
    // that each weaker key would join to a grouped declaration it is not:
    // the field `Square` inside the struct `Square` shares name and line,
    // `Board`'s field `Red` shares kind and name with the grouped `Red`, and
    // `sub/docs.go` declares an ungrouped `Red` on the same line, under a
    // directory `surface` walks and this `map` does not.
    //
    // Derived through `--no-private`, because `surface` drops unexported
    // symbols; without that filter the first unexported member the
    // fixture gains fails this test on correct resolver behaviour.
    /// Both payloads spell these fields differently (`name` / `start_line`
    /// against `source_name` / `source_line`), so a rename on either side
    /// is a live risk. Panic on the missing field rather than defaulting:
    /// defaulted keys empty the join and surface as "surface is missing
    /// members", which is the one assertion here that cannot name a cause.
    fn key(
        kind: &serde_json::Value,
        name: &serde_json::Value,
        line: &serde_json::Value,
    ) -> (String, String, u64) {
        (
            kind.as_str().expect("kind").to_string(),
            name.as_str().expect("name").to_string(),
            line.as_u64().expect("line"),
        )
    }
    type Keys = std::collections::HashSet<(String, String, u64)>;

    let v: serde_json::Value =
        serde_json::from_str(&run(&["map", FIXTURE, "--no-private", "--json"])).expect("json");

    /// Collects the members of documented blocks into `out`, and the ones
    /// among them that also carry a comment of their own into `self_documented`.
    fn walk(list: &[serde_json::Value], out: &mut Keys, self_documented: &mut Keys) {
        for d in list {
            if !docs(d, "group").is_empty() {
                let k = key(&d["kind"], &d["name"], &d["start_line"]);
                if !docs(d, "docs").is_empty() {
                    self_documented.insert(k.clone());
                }
                out.insert(k);
            }
            if let Some(children) = d["children"].as_array() {
                walk(children, out, self_documented);
            }
        }
    }
    let mut in_a_documented_block = Keys::new();
    let mut self_documented = Keys::new();
    walk(
        v["files"][0]["declarations"]
            .as_array()
            .expect("declarations"),
        &mut in_a_documented_block,
        &mut self_documented,
    );
    // Copying `group` only where `docs` is empty passes for every member
    // except one that carries both, so the fixture has to keep one.
    assert!(
        !self_documented.is_empty(),
        "fixture must keep a member with its own doc comment inside a documented block: {in_a_documented_block:?}"
    );

    let dir = std::path::Path::new(FIXTURE).parent().unwrap();
    let out = run(&["surface", dir.to_str().unwrap(), "--json"]);
    let surfaced: serde_json::Value = serde_json::from_str(&out).expect("json");

    let fixture_path = std::path::Path::new(FIXTURE);
    let mut seen = Keys::new();
    for e in surfaced["entries"].as_array().expect("entries") {
        let from_fixture = e["source_path"]
            .as_str()
            .is_some_and(|p| std::path::Path::new(p) == fixture_path);
        if !from_fixture {
            continue;
        }
        let k = key(&e["kind"], &e["source_name"], &e["source_line"]);
        if !in_a_documented_block.contains(&k) {
            continue;
        }
        seen.insert(k.clone());
        assert!(
            !docs(e, "group").is_empty(),
            "{} {} is a member of a documented block but surface reports no group:\n{e:#?}",
            k.0,
            k.1
        );
    }
    assert_eq!(
        seen, in_a_documented_block,
        "surface must report every member `map` puts in a documented block"
    );
}

#[test]
fn two_blocks_whose_comments_read_the_same_stay_two_blocks() {
    // `// Deprecated: …` and generator banners repeat verbatim above
    // consecutive blocks, so the renderer cannot tell one block from the
    // next by its prose.
    let s = run(&["map", FIXTURE]);
    assert_eq!(
        s.matches("[group] // Deprecated: use Standalone instead.")
            .count(),
        2,
        "adjacent blocks with the same comment rendered as one:\n{s}"
    );
    let v = json();
    let d = decls(&v);
    assert_ne!(
        d["Ancient"]["group"]["start_line"], d["Obsolete"]["group"]["start_line"],
        "two blocks must not share one group"
    );
}

#[test]
fn every_line_of_a_block_comment_carries_the_marker() {
    // One `/* … */` comment is a single `docs` entry spanning several
    // lines. An unmarked continuation reads as the following symbol's own.
    let s = run(&["map", FIXTURE]);
    for line in [
        "/* Codes are the wire codes.",
        "Adding one is a protocol change.",
        "*/",
    ] {
        assert!(
            s.contains(&format!("[group] {line}")),
            "block-comment line {line:?} rendered without the marker:\n{s}"
        );
    }
}

#[test]
fn no_lines_keeps_a_group_distinguishable() {
    // `--no-lines` drops line numbers, but the range is the only thing a
    // block can be identified by, so dropping it would put two blocks
    // sharing a comment back where issue #46 found them.
    let v: serde_json::Value =
        serde_json::from_str(&run(&["map", FIXTURE, "--no-lines", "--json"])).expect("json");
    let d = decls(&v);
    assert_ne!(
        d["Ancient"]["group"], d["Obsolete"]["group"],
        "two blocks serialised identically under --no-lines"
    );
}

#[test]
fn a_group_reports_the_lines_of_its_block() {
    let v = json();
    let d = decls(&v);
    // The comment above `const (` matches no declaration's line, so a
    // consumer deciding "is this comment a doc comment" by position needs
    // the block's own range (issue #46).
    let group = &d["Blue"]["group"];
    assert_eq!(
        group["start_line"], 6,
        "block starts at `const (`:\n{group:#?}"
    );
    assert_eq!(group["end_line"], 10, "block ends at `)`:\n{group:#?}");
}

#[test]
fn no_docs_sheds_the_group_too() {
    let out = run(&["map", FIXTURE, "--no-docs", "--json"]);
    assert!(
        !out.contains("\"group\""),
        "--no-docs must shed a group's comment the way it sheds `docs`:\n{out}"
    );
}

#[test]
fn a_groups_deprecation_reaches_its_members() {
    let s = run(&["digest", FIXTURE, "--include-fields"]);
    assert!(
        s.contains("Ancient [field] [deprecated]"),
        "`Deprecated:` on a group must deprecate its members:\n{s}"
    );
}
