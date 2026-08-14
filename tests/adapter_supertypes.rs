//! Every adapter records the supertypes its language spells out.
//!
//! Five did not, each because it asked the grammar for the wrong thing and
//! got nothing back while the *signature* still showed the clause, which is
//! what kept the loss invisible in `map` output. These tests are about the
//! adapters rather than about resolution, so they carry their own fixtures
//! and stay out of `implements_collision.rs`.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

fn fixture(name: &str) -> String {
    format!("tests/fixtures/implements_collision/{name}")
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

/// Every language whose adapter claims to record supertypes actually does,
/// for a clause harder than the simplest one.
///
/// Five adapters recorded none, each because it asked the grammar for the
/// wrong thing and got nothing back while the *signature* still showed the
/// clause, which is what kept the loss invisible. PHP and C++ read a
/// `field()` the grammar exposes as an ordinary child; JavaScript was
/// descended one level too far, having no `extends_clause` wrapper; Ruby
/// kept the `<` in the name; Kotlin looked for a container node the grammar
/// does not emit. Each answered 0 for every type in that language.
///
/// Each fixture carries a second type whose clause is harder than `: Root`
/// — generic, qualified, several bases at once — because one unqualified
/// base is exactly the shape that let two PHP defects ship green.
#[test]
fn every_adapter_records_its_supertypes() {
    let root = fixture("languages");
    // The list is checked against the directory rather than trusted, so a
    // fixture added without a line here fails instead of going unrun.
    let mut on_disk: Vec<String> = std::fs::read_dir(&root)
        .expect("fixture dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    on_disk.sort();
    let langs = [
        "cpp",
        "csharp",
        "go",
        "java",
        "javascript",
        "kotlin",
        "php",
        "python",
        "ruby",
        "rust",
        "scala",
        "typescript",
    ];
    assert_eq!(on_disk, langs, "the fixture set and the list disagree");

    for lang in langs {
        let dir = format!("{root}/{lang}");
        let (code, stdout, stderr) = run(&["implements", "Root", &dir]);
        assert_eq!(code, Some(0), "{lang}: {stderr}");
        let names: Vec<&str> = stdout
            .lines()
            .skip(1)
            .filter_map(|l| l.split_whitespace().nth(2))
            .collect();
        assert!(
            names.contains(&"Leaf"),
            "{lang}: the plain clause records no supertype:\n{stdout}"
        );
        // Ruby's harder case is a scope-resolved superclass of its own, and
        // JavaScript's is a qualified base; both still reach `Root` only
        // where the fixture says so.
        if !matches!(lang, "ruby") {
            assert!(
                names.contains(&"Hard"),
                "{lang}: the harder clause records no supertype:\n{stdout}"
            );
        }
        assert!(
            !names
                .iter()
                .any(|n| n.contains('<') || n.starts_with("impl_")),
            "{lang}: a synthetic or generic-spelled name reached the answer:\n{stdout}"
        );
    }

    // Ruby's `Hard` inherits through a scope resolution, with a comment
    // sitting between the operator and the name.
    let (_, stdout, _) = run(&["implements", "Nested", &format!("{root}/ruby")]);
    assert!(stdout.contains("Hard"), "ruby scope resolution:\n{stdout}");

    // PHP's `Hard` reaches `Root` through `Leaf`, so only a second target
    // exercises its `namespace\Other` clause — the relative spelling, which
    // is a `relative_name` node rather than a `name` or a `qualified_name`.
    let (_, stdout, _) = run(&["implements", "Other", &format!("{root}/php")]);
    assert!(
        stdout.contains("Hard"),
        "php namespace-relative clause:\n{stdout}"
    );
}

/// Recording a base changes the *signature* too, for the three languages
/// that build it from `bases` — so `map`, `digest`, `show` and `surface`
/// print a clause they did not print before. That is the intended
/// consequence of the fix and it is pinned here rather than discovered.
#[test]
fn a_recorded_base_reaches_the_signature() {
    let root = fixture("languages");
    for (lang, file, want) in [
        ("php", "a.php", "class Leaf implements Root"),
        // `public` is an access specifier, not a type, so it stays out of
        // `bases` and therefore out of the signature the join builds.
        ("cpp", "a.cpp", "class Leaf : Root"),
        ("ruby", "a.rb", "class Leaf < Root"),
    ] {
        let (code, stdout, stderr) = run(&["map", &format!("{root}/{lang}/{file}")]);
        assert_eq!(code, Some(0), "{lang}: {stderr}");
        assert!(
            stdout.contains(want),
            "{lang}: the signature lost its supertype clause, expected {want:?}:\n{stdout}"
        );
    }

    // And nothing that is not a type reaches `bases`. A TypeScript type
    // argument sits beside its base in the clause rather than inside it, so
    // `extends Box<T>` used to record `<T>` as a base of its own — invisible
    // to `implements`, which normalises it to nothing, but printed in the
    // signature and carried to every other consumer of `bases`.
    let (code, stdout, stderr) = run(&[
        "map",
        &format!("{root}/typescript/a.ts"),
        "--json",
        "--compact",
    ]);
    assert_eq!(code, Some(0), "{stderr}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let mut seen = 0;
    for file in doc["files"].as_array().expect("files") {
        for decl in file["declarations"].as_array().expect("declarations") {
            for base in decl["bases"].as_array().unwrap_or(&Vec::new()) {
                let base = base.as_str().expect("string");
                assert!(
                    !base.starts_with('<'),
                    "a type argument was recorded as a base: {base:?}"
                );
                seen += 1;
            }
        }
    }
    assert!(seen >= 3, "the fixture recorded no bases to check ({seen})");
}
