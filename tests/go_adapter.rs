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
//!      (issue #47) — `docs: null` there used to be indistinguishable
//!      from a genuinely undocumented field.
//!   4. A trailing comment belongs to the declaration it sits beside, not
//!      to the one on the next line.

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
    serde_json::from_str(&run(&["map", FIXTURE, "--json"])).expect("json")
}

/// Every declaration in the file, flattened, keyed by name.
fn decls(v: &serde_json::Value) -> std::collections::HashMap<String, serde_json::Value> {
    fn walk(
        list: &[serde_json::Value],
        out: &mut std::collections::HashMap<String, serde_json::Value>,
    ) {
        for d in list {
            out.insert(
                d["name"].as_str().unwrap_or_default().to_string(),
                d.clone(),
            );
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

/// Every member of a documented block stays visually attached to the
/// `[group]` line documenting it. The three block kinds are the axis: a
/// `const` / `var` group renders its members as consecutive one-liners,
/// while a `type` group's members each push a trailing blank line, which
/// is the case that separated a later member from its marker.
#[test]
fn a_groups_members_are_not_split_from_its_marker() {
    let s = run(&["map", FIXTURE]);
    let mut checked = 0;
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
        // The renderer pushes `String::new()` for a separator, so a blank
        // line inside the region is exactly "\n\n". Scanning `lines()`
        // instead would count the indent before `last_member` as blank.
        assert!(
            !s[from..from + to].contains("\n\n"),
            "a blank line splits {last_member:?} from {marker:?}:\n{s}"
        );
        checked += 1;
    }
    assert_eq!(checked, 3, "all three block kinds must be exercised");
    // The blank line still separates the block from what follows it.
    assert!(
        s.contains("type Square struct  L46\n\n"),
        "the group's own trailing blank line was swallowed:\n{s}"
    );

    // With the markers hidden there is nothing for a closed gap to attach
    // a member to, so a documented block has to render exactly like an
    // undocumented one. Otherwise the spacing alone tells the reader which
    // block carries the comment they asked to hide.
    let hidden = run(&["map", FIXTURE, "--no-docs"]);
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
    let dir = std::path::Path::new(FIXTURE).parent().unwrap();
    let out = run(&["surface", dir.to_str().unwrap(), "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    let mut members = 0;
    for e in v["entries"].as_array().expect("entries") {
        let name = e["source_name"].as_str().unwrap_or_default();
        // Every block member in the fixture, across all three block kinds.
        if ![
            "Red", "Blue", "MaxBytes", "MaxHops", "Ancient", "Obsolete", "CodeOK", "Square",
        ]
        .contains(&name)
        {
            continue;
        }
        members += 1;
        assert!(
            !docs(e, "group").is_empty(),
            "{name} is a member of a documented block but surface reports no group:\n{e:#?}"
        );
    }
    assert_eq!(members, 8, "every block member must be exercised");
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
