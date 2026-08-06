//! A markdown file's leading `---…---` YAML block is structure, not prose.
//! For a task card or a Jekyll / Hugo / Astro / Obsidian page the frontmatter
//! is often the whole content, and a file like that used to map as empty.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let out = Command::new(bin())
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .expect("run ast-bro");
    (
        out.status.code(),
        String::from_utf8(out.stdout).expect("utf8"),
        String::from_utf8(out.stderr).expect("utf8"),
    )
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("card.md"),
        "---\nid: card-1\ntype: task\nstatus: open\n---\n\n# Heading\n\nbody\n",
    )
    .unwrap();
    // Everything this file has to say lives in its frontmatter.
    std::fs::write(
        dir.path().join("only.md"),
        "---\nid: card-2\nstatus: done\n---\n",
    )
    .unwrap();
    // A `---` that is not on line 1 is a horizontal rule, not metadata.
    std::fs::write(dir.path().join("rule.md"), "text\n\n---\n\nmore\n").unwrap();
    // An unterminated fence has no knowable extent.
    std::fs::write(dir.path().join("open.md"), "---\nid: x\n\n# Heading\n").unwrap();
    // Windows line endings: the fence check and the byte range must both hold.
    std::fs::write(
        dir.path().join("crlf.md"),
        "---\r\nid: card-3\r\nstatus: open\r\n---\r\n\r\n# Heading\r\n",
    )
    .unwrap();
    // The alternative YAML document terminator.
    std::fs::write(dir.path().join("dots.md"), "---\nid: card-4\n...\n\n# H\n").unwrap();
    // A UTF-8 BOM sits *before* the fence; editors on Windows add one.
    std::fs::write(
        dir.path().join("bom.md"),
        "\u{feff}---\nid: card-5\nstatus: open\n---\n\n# Heading\n",
    )
    .unwrap();
    dir
}

#[test]
fn frontmatter_appears_in_map_with_its_line_range() {
    let dir = fixture();
    let card = dir.path().join("card.md");
    let (code, stdout, stderr) = run(&["map", card.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("--- frontmatter") && stdout.contains("L1-5"),
        "stdout:\n{stdout}"
    );
    assert!(stdout.contains("1 frontmatter"), "header count:\n{stdout}");
}

#[test]
fn a_frontmatter_only_file_no_longer_maps_as_empty() {
    let dir = fixture();
    let only = dir.path().join("only.md");
    let (code, stdout, stderr) = run(&["map", only.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("--- frontmatter") && stdout.contains("L1-4"),
        "stdout:\n{stdout}"
    );
}

#[test]
fn frontmatter_survives_the_digest_preset() {
    // Under `digest` the file's only line would otherwise be dropped, which
    // is exactly the "renders empty" bug.
    let dir = fixture();
    let (code, stdout, stderr) = run(&["digest", dir.path().to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(stdout.contains("--- frontmatter"), "stdout:\n{stdout}");
    assert!(
        stdout.matches("--- frontmatter").count() >= 2,
        "both card.md and only.md have frontmatter:\n{stdout}"
    );
}

#[test]
fn a_horizontal_rule_is_not_frontmatter() {
    let dir = fixture();
    let rule = dir.path().join("rule.md");
    let (code, stdout, stderr) = run(&["map", rule.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        !stdout.contains("frontmatter"),
        "a `---` below line 1 stays a thematic break:\n{stdout}"
    );
}

#[test]
fn an_unterminated_fence_is_not_claimed() {
    let dir = fixture();
    let open = dir.path().join("open.md");
    let (code, stdout, stderr) = run(&["map", open.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        !stdout.contains("frontmatter"),
        "an unclosed fence has no knowable extent:\n{stdout}"
    );
}

#[test]
fn frontmatter_is_a_show_handle_with_values_intact() {
    let dir = fixture();
    let card = dir.path().join("card.md");
    let (code, stdout, stderr) = run(&["show", card.to_str().unwrap(), "frontmatter"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("status: open") && stdout.contains("id: card-1"),
        "the raw block, values included:\n{stdout}"
    );
}

#[test]
fn one_call_collects_every_cards_frontmatter() {
    // Only reachable because `show` takes a directory now.
    let dir = fixture();
    let (code, stdout, stderr) = run(&["show", dir.path().to_str().unwrap(), "frontmatter"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    // Every card in the fixture that *has* frontmatter, in one call — asserted
    // by content rather than by count, so adding a fixture can't make this
    // fail without a real behaviour change.
    let ids = ["card-1", "card-2", "card-3", "card-4", "card-5"];
    for id in ids {
        assert!(stdout.contains(id), "{id} missing from:\n{stdout}");
    }
    // The matched-file count is tied to the id list rather than written out,
    // so adding a card updates both halves of the claim at once.
    assert!(
        stdout.contains(&format!(
            "match(es) for 'frontmatter' in {} of ",
            ids.len()
        )),
        "coverage header should name every matching file:\n{stdout}"
    );
}

#[test]
fn crlf_frontmatter_is_detected_and_bounded_correctly() {
    let dir = fixture();
    let crlf = dir.path().join("crlf.md");
    let (code, stdout, stderr) = run(&["map", crlf.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("--- frontmatter") && stdout.contains("L1-4"),
        "a CRLF fence must close on line 4:\n{stdout}"
    );

    // The extracted block must end at the closing fence — no stray blank
    // line, no carriage return left in the rendered text.
    let (code, body, stderr) = run(&["show", crlf.to_str().unwrap(), "frontmatter"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let last = body.trim_end_matches('\n').lines().last().unwrap();
    assert_eq!(last.trim_end_matches('\r'), "---", "body:\n{body:?}");
    assert!(body.contains("card-3"), "body:\n{body}");
}

#[test]
fn a_bom_does_not_push_the_fence_off_line_one() {
    let dir = fixture();
    let bom = dir.path().join("bom.md");
    let (code, stdout, stderr) = run(&["map", bom.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("--- frontmatter") && stdout.contains("L1-4"),
        "a BOM sits before the fence, which is still on line 1:\n{stdout}"
    );

    // The extracted block starts at the fence, not at the BOM.
    let (code, body, stderr) = run(&["show", bom.to_str().unwrap(), "frontmatter"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(!body.contains('\u{feff}'), "BOM leaked into the block:\n{body:?}");
    assert!(body.contains("card-5"), "body:\n{body}");
}

#[test]
fn a_dot_terminator_closes_the_block() {
    let dir = fixture();
    let dots = dir.path().join("dots.md");
    let (code, stdout, stderr) = run(&["map", dots.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stdout.contains("--- frontmatter") && stdout.contains("L1-3"),
        "stdout:\n{stdout}"
    );
}

#[test]
fn json_reports_the_frontmatter_kind() {
    let dir = fixture();
    let card = dir.path().join("card.md");
    let (code, stdout, stderr) = run(&["map", card.to_str().unwrap(), "--json", "--compact"]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    let decls = doc["files"][0]["declarations"]
        .as_array()
        .expect("declarations");
    assert_eq!(decls[0]["kind"], "frontmatter");
    assert_eq!(decls[0]["name"], "frontmatter");
    assert_eq!(decls[0]["start_line"], 1);
    assert_eq!(decls[0]["end_line"], 5);
}
