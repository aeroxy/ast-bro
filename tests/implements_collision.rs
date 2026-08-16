//! `implements` resolves a supertype reference to one declaration.
//!
//! Two packages may declare the same simple name. Matching a supertype
//! reference against that name alone merges the two declarations, and every
//! subtype of either is then reported as a subtype of both. The fixture
//! under `tests/fixtures/implements_collision` builds that shape in five
//! languages: `TextCodec` is declared twice, one of them inside the
//! `BinaryCodec` hierarchy and one outside it, and `PrimitiveText` extends
//! the outside one. Before the fix `PrimitiveText` was reported as a
//! `BinaryCodec` in every language that reported anything at all.
//!
//! The languages are not interchangeable here, because each reaches the
//! answer by a different rule. C#, Java and Kotlin pick by namespace, and
//! the latter two also have to let a single-type import outrank the file's
//! own package. Python and TypeScript have no namespace, so their answer
//! comes from the import alone.

use std::path::PathBuf;
use std::process::Command;

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

fn fixture(lang: &str) -> String {
    format!("tests/fixtures/implements_collision/{lang}")
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

/// One `implements` call over the MCP transport, returning the raw
/// response line. MCP has a single channel, so a qualification the CLI
/// sends to stderr has to ride on the body here.
fn mcp_implements(target: &str, path: &str) -> String {
    let req = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"implements","arguments":{{"target":"{target}","paths":["{path}"]}}}}}}"#
    );
    let out = Command::new(bin())
        .arg("mcp")
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .expect("stdin")
                .write_all(format!("{req}\n").as_bytes())?;
            child.wait_with_output()
        })
        .expect("run mcp");
    String::from_utf8(out.stdout).expect("utf8")
}

/// The rendered text of that response. The transport puts the whole body on
/// one JSON line, so counting lines on the raw response counts one.
fn mcp_text(target: &str, path: &str) -> String {
    let raw = mcp_implements(target, path);
    let doc: serde_json::Value = serde_json::from_str(raw.trim()).expect("valid json-rpc");
    doc["result"]["content"][0]["text"]
        .as_str()
        .expect("text content")
        .to_string()
}

/// Every language declares `TextCodec` twice and hangs `PrimitiveText` off
/// the copy that has nothing to do with `BinaryCodec`.
#[test]
fn an_unrelated_subtree_stays_out_of_the_closure() {
    for lang in ["java", "csharp", "kotlin", "python", "typescript"] {
        let (code, stdout, _) = run(&["implements", "BinaryCodec", &fixture(lang)]);
        assert_eq!(code, Some(0), "{lang}");
        assert!(
            stdout.contains("AbstractCodec") && stdout.contains("TextCodec"),
            "{lang}: the real closure went missing:\n{stdout}"
        );
        assert!(
            !stdout.contains("PrimitiveText"),
            "{lang}: a subtype of the other TextCodec joined the closure:\n{stdout}"
        );
    }
}

/// C# `using Example.Api;` names a namespace, and Python's `from .base
/// import X` names a sibling file. Both are the only thing that can decide
/// between two same-named declarations when neither the qualifier nor the
/// referring scope does.
#[test]
fn a_namespace_using_and_a_relative_import_both_decide() {
    let (code, stdout, _) = run(&["implements", "Example.Api.TextCodec", &fixture("csharp")]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("UsingCodec"),
        "the C# using directive did not select its namespace:\n{stdout}"
    );

    let (code, stdout, _) = run(&["implements", "BinaryCodec", &fixture("python")]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("BoxCodec"),
        "the relative import did not select the sibling module:\n{stdout}"
    );
}

/// A single-type import outranks the referring file's own package (JLS
/// 6.4.1, and Kotlin's rule too). `BoxCodec` sits in `org.example.jdbc`,
/// which declares its own `TextCodec`, and still means the one it imported
/// from `org.example.api`.
#[test]
fn an_import_outranks_the_files_own_package() {
    for lang in ["java", "kotlin"] {
        let (code, stdout, _) = run(&["implements", "org.example.api.TextCodec", &fixture(lang)]);
        assert_eq!(code, Some(0), "{lang}");
        assert!(
            stdout.contains("BoxCodec"),
            "{lang}: the imported interface lost to the same-package one:\n{stdout}"
        );

        // The other side of the same claim: nothing extends the class in
        // `jdbc`, and `BoxCodec` writing the bare name does not make it one.
        let (code, stdout, _) = run(&["implements", "org.example.jdbc.TextCodec", &fixture(lang)]);
        assert_eq!(
            code,
            Some(0),
            "{lang}: an existing type with no subtypes exits 0"
        );
        assert!(
            stdout.contains("0 match(es)") && !stdout.contains("BoxCodec"),
            "{lang}: the same-package name captured an imported reference:\n{stdout}"
        );
    }
}

/// Kotlin reported no supertypes at all until the adapter stopped looking
/// for a `delegation_specifiers` container the grammar does not emit, so
/// `implements` had no Kotlin edges to walk and answered 0 for everything.
#[test]
fn kotlin_records_its_supertypes() {
    let (code, stdout, _) = run(&["implements", "Codec", "--direct", &fixture("kotlin")]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("BinaryCodec") && stdout.contains("TextCodec"),
        "kotlin supertypes are missing from the IR:\n{stdout}"
    );
}

/// A qualifier that fits no declaration is a rejected query, not an answer
/// for every declaration of the bare name.
#[test]
fn an_unmatched_qualifier_is_rejected() {
    let (code, stdout, stderr) = run(&["implements", "no.such.TextCodec", &fixture("java")]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(
        stdout.is_empty(),
        "a rejected call writes nothing to stdout"
    );
    assert!(stderr.contains("no type named"), "{stderr}");
}

/// A reference the resolver could not pin is a dropped edge, and a dropped
/// edge is reported rather than guessed at — the reasoning `--limit` and
/// `--depth` already follow. `Orphan` lives in a third package that neither
/// declares `TextCodec` nor imports one.
#[test]
fn an_unpinnable_reference_is_reported_not_guessed() {
    let (code, stdout, stderr) = run(&["implements", "BinaryCodec", &fixture("java")]);
    assert_eq!(code, Some(0), "a qualified answer is still an answer");
    assert!(
        !stdout.contains("Orphan"),
        "an unpinnable edge was taken anyway:\n{stdout}"
    );
    assert!(
        stderr.contains("Orphan") && stderr.contains("left out of the answer"),
        "the dropped edge went unreported:\n{stderr}"
    );
    // Both candidates named, each with where it is declared: in a language
    // with no namespace the qualified names alone are identical.
    assert!(
        stderr.contains("api/TextCodec.java") && stderr.contains("jdbc/TextCodec.java"),
        "the note does not say which declarations collided:\n{stderr}"
    );
}

/// `--json` carries the same qualifications, so a machine consumer does not
/// have to parse the stderr notes.
#[test]
fn json_carries_the_dropped_edges() {
    let (code, stdout, _) = run(&[
        "implements",
        "BinaryCodec",
        &fixture("java"),
        "--json",
        "--compact",
    ]);
    assert_eq!(code, Some(0));
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    let ambiguous = doc["ambiguous"].as_array().expect("ambiguous array");
    assert_eq!(ambiguous.len(), 1, "{stdout}");
    assert_eq!(ambiguous[0]["name"], "Orphan");
    assert_eq!(ambiguous[0]["base"], "TextCodec");
    assert_eq!(
        ambiguous[0]["candidates"]
            .as_array()
            .expect("candidates")
            .len(),
        2
    );
    assert!(
        doc["matches"]
            .as_array()
            .expect("matches")
            .iter()
            .all(|m| m["name"] != "PrimitiveText"),
        "{stdout}"
    );
}

/// An unqualified target that several packages declare answers for all of
/// them, and says so — the alternative is picking one silently.
#[test]
fn an_ambiguous_target_is_answered_for_and_reported() {
    let (code, stdout, stderr) = run(&["implements", "TextCodec", &fixture("java")]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("PrimitiveText") && stdout.contains("BoxCodec"),
        "the answer dropped one of the two hierarchies:\n{stdout}"
    );
    assert!(
        stderr.contains("is declared in 2 places"),
        "the merged target went unreported:\n{stderr}"
    );
}

/// MCP has one channel, so the notes ride on the response body instead of
/// stderr. Same wording either way.
#[test]
fn mcp_puts_the_same_notes_in_the_body() {
    let body = mcp_implements("BinaryCodec", &fixture("java"));
    assert!(
        body.contains("Orphan") && body.contains("left out of the answer"),
        "the note is missing from the MCP body:\n{body}"
    );
    assert!(
        !body.contains("PrimitiveText"),
        "MCP took the unpinnable edge:\n{body}"
    );
}

/// The supertype the caller asks about is very often not in the walk at
/// all: `implements Runnable` over an application tree means the JDK's.
/// Rooting the closure on a resolved declaration dropped every one of those
/// answers, and the whole suite stayed green because nothing pinned it.
#[test]
fn a_supertype_outside_the_walk_is_still_a_target() {
    let (code, stdout, _) = run(&["implements", "Runnable", &fixture("external")]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("class A") && stdout.contains("class B"),
        "the closure of an out-of-walk supertype went missing:\n{stdout}"
    );

    // `--direct` still means one level.
    let (_, stdout, _) = run(&["implements", "Runnable", "--direct", &fixture("external")]);
    assert!(
        stdout.contains("class A") && !stdout.contains("class B"),
        "--direct walked past the first level:\n{stdout}"
    );

    // A name nothing declares and nothing references is still a rejection.
    let (code, _, stderr) = run(&["implements", "ZzNoSuchType", &fixture("external")]);
    assert_eq!(code, Some(2), "{stderr}");
}

/// Two qualified spellings that disagree name different types, even when
/// neither is in the walk. A bare spelling contradicts neither.
#[test]
fn a_qualified_external_target_respects_the_qualifier() {
    let (code, stdout, _) = run(&["implements", "mylib.Runnable", &fixture("external")]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("class A"),
        "a bare reference should answer a qualified target:\n{stdout}"
    );
    assert!(
        !stdout.contains("class C"),
        "'otherlib.Runnable' is not 'mylib.Runnable':\n{stdout}"
    );
}

/// A renamed import is the one case where the name in the inheritance
/// clause is not a declaration name anywhere, so the lookup misses and only
/// the import can say what was meant.
#[test]
fn a_renamed_import_still_names_its_target() {
    for lang in ["kotlin", "csharp", "rust"] {
        let dir = format!("{}/{lang}", fixture("rename"));
        let (code, stdout, _) = run(&["implements", "Base", &dir]);
        assert_eq!(code, Some(0), "{lang}");
        assert!(
            stdout.contains("Child"),
            "{lang}: the rename hid the edge:\n{stdout}"
        );
    }
}

/// Enums and C# delegates are declared types. Answering "no subtypes" for
/// one is a real answer; rejecting the query is not — and the qualified
/// spelling has to agree with the bare one.
#[test]
fn every_declared_kind_can_be_a_target() {
    for target in ["Color", "app.Color", "Handler", "App.Handler"] {
        let (code, stdout, stderr) = run(&["implements", target, &fixture("kinds")]);
        assert_eq!(code, Some(0), "{target} was rejected:\n{stderr}");
        assert!(stdout.contains("0 match(es)"), "{target}:\n{stdout}");
    }
    // And an enum belongs in a closure as a subtype, which is what it is.
    let (_, stdout, _) = run(&["implements", "Paint", &fixture("kinds")]);
    assert!(
        stdout.contains("enum Color"),
        "an enum implementing an interface is an implementor:\n{stdout}"
    );
}

/// The note is a qualification, not the answer, so its list is capped like
/// every other. Three cases, because a threshold has three: below it, on
/// it, and above it — the middle one is where a cap that fires on
/// "strictly more" quietly does not.
#[test]
fn the_dropped_edge_note_is_capped() {
    const SHOWN: usize = 10;
    for orphans in [SHOWN - 1, SHOWN, SHOWN + 1] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        std::fs::create_dir_all(dir.join("api")).expect("mkdir");
        std::fs::create_dir_all(dir.join("jdbc")).expect("mkdir");
        std::fs::create_dir_all(dir.join("other")).expect("mkdir");
        std::fs::write(
            dir.join("api/Widget.java"),
            "package api;\npublic interface Widget {}\n",
        )
        .expect("write");
        std::fs::write(
            dir.join("jdbc/Widget.java"),
            "package jdbc;\npublic interface Widget {}\n",
        )
        .expect("write");
        for i in 0..orphans {
            std::fs::write(
                dir.join(format!("other/Orphan{i}.java")),
                format!("package other;\npublic interface Orphan{i} extends Widget {{}}\n"),
            )
            .expect("write");
        }
        let (code, _, stderr) = run(&["implements", "api.Widget", dir.to_str().expect("utf8")]);
        assert_eq!(code, Some(0));

        // The header always reports the true total, capped or not.
        assert!(
            stderr.contains(&format!("# note: {orphans} supertype reference(s)")),
            "orphans={orphans}: wrong total:\n{stderr}"
        );
        let listed = stderr.lines().filter(|l| l.contains("Orphan")).count();
        assert_eq!(
            listed,
            orphans.min(SHOWN),
            "orphans={orphans}: wrong number of detail lines:\n{stderr}"
        );
        assert_eq!(
            stderr.contains("more (the full list is in --json)"),
            orphans > SHOWN,
            "orphans={orphans}: the overflow line disagrees with the cap:\n{stderr}"
        );

        // JSON is uncapped — the note points at it, so it has to be.
        let (_, stdout, _) = run(&[
            "implements",
            "api.Widget",
            dir.to_str().expect("utf8"),
            "--json",
            "--compact",
        ]);
        let doc: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
        assert_eq!(
            doc["ambiguous"].as_array().expect("ambiguous").len(),
            orphans,
            "orphans={orphans}: json dropped edges:\n{stdout}"
        );

        // MCP has one channel, so the same cap has to hold on the body.
        let body = mcp_text("api.Widget", dir.to_str().expect("utf8"));
        assert_eq!(
            body.lines().filter(|l| l.contains("Orphan")).count(),
            orphans.min(SHOWN),
            "orphans={orphans}: the MCP body is not capped:\n{body}"
        );
    }
}

/// A qualifier is checked whenever one is written, however many candidates
/// carry the simple name. Gating it on "two or more" made the single-
/// candidate case the silent one: `implements java.lang.Runnable` was
/// rejected outright, and `mylib.Runnable` claimed the class that spelled
/// out the JDK's — with one candidate there is no second one to make the
/// merge visible.
#[test]
fn one_local_declaration_does_not_capture_a_qualified_reference() {
    let dir = fixture("external_shadowed");

    let (code, stdout, _) = run(&["implements", "java.lang.Runnable", &dir]);
    assert_eq!(code, Some(0), "a qualified out-of-walk target was rejected");
    assert!(
        stdout.contains("class D") && !stdout.contains("class E"),
        "the JDK's Runnable answered for the wrong classes:\n{stdout}"
    );

    let (code, stdout, _) = run(&["implements", "mylib.Runnable", &dir]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("class E") && !stdout.contains("class D"),
        "the local Runnable claimed a reference that names another:\n{stdout}"
    );
}

/// A rename shadows a declaration of the same name elsewhere, in the
/// resolver as in the language, and the alias syntax is recognised however
/// it is spaced — `using Alias=A.B.Base;` is valid C#.
#[test]
fn a_rename_outranks_a_same_named_declaration() {
    let dir = format!("{}/csharp", fixture("rename"));

    let (code, stdout, _) = run(&["implements", "Base", &dir]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("Child") && stdout.contains("Compact"),
        "a rename lost to a same-named declaration, or the compact spelling \
         was not recognised:\n{stdout}"
    );

    let (code, stdout, _) = run(&["implements", "Zz.Alias", &dir]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("0 match(es)"),
        "the declaration named Alias captured the renamed references:\n{stdout}"
    );
}

/// A bare out-of-walk target covers every spelling of the name, and those
/// spellings can name different types. Merging them is the defect this
/// resolver exists to stop, so the answer reports what it merged.
#[test]
fn a_bare_external_target_reports_the_spellings_it_merged() {
    let (code, stdout, stderr) = run(&["implements", "Runnable", &fixture("external")]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("class A") && stdout.contains("class C") && stdout.contains("class F"),
        "{stdout}"
    );
    assert!(
        stderr.contains("written 2 different ways")
            && stderr.contains("otherlib.Runnable")
            && stderr.contains("mylib.Runnable"),
        "the merged spellings went unreported:\n{stderr}"
    );

    // A bare spelling beside a qualified one is ordinary mixed style within
    // one language, not two types. Counting it would fire this note on most
    // repositories and advise a split that drops half the answer.
    let (_, _, stderr) = run(&[
        "implements",
        "Runnable",
        &format!("{}/A.java", fixture("external")),
        &format!("{}/C.java", fixture("external")),
    ]);
    assert!(
        !stderr.contains("different ways"),
        "bare plus qualified was reported as a merge:\n{stderr}"
    );
}

/// The guard still has to discriminate, or it is not a guard: Java has no
/// import rename in any form, so a Java file never takes the rename path
/// however its `=` is spaced.
#[test]
fn the_rename_guard_ignores_ordinary_assignments() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    std::fs::write(
        dir.join("Api.java"),
        "package api;\npublic interface Base {}\n",
    )
    .expect("write");
    // No import renames anything here; the `=` is ordinary code.
    std::fs::write(
        dir.join("Child.java"),
        "package app;\n\nimport api.Base;\n\npublic class Child implements Base {\n  \
         private int n = 1;\n}\n",
    )
    .expect("write");
    let (code, stdout, _) = run(&["implements", "Base", dir.to_str().expect("utf8")]);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("Child"), "{stdout}");
}

/// A qualifier can only rule out a declaration it can be compared with.
/// Where the declaration knows no namespace, `api.Base` is as likely a
/// module alias as a package path, and treating a comparison that cannot be
/// made as a disagreement deleted the edge — with no ambiguity to report,
/// since one candidate leaves nothing to be ambiguous about.
#[test]
fn a_qualifier_the_walk_cannot_evaluate_does_not_delete_the_edge() {
    for lang in ["typescript", "python", "csharp"] {
        let dir = format!("{}/{lang}", fixture("alias_qualifier"));
        let (code, stdout, _) = run(&["implements", "Base", &dir]);
        assert_eq!(code, Some(0), "{lang}");
        assert!(
            stdout.contains("Child"),
            "{lang}: a module-alias qualifier deleted the edge:\n{stdout}"
        );
    }
}

/// A qualifier that fits several declarations narrows rather than decides,
/// so the rules below it still have to run. `Outer.Inner` fits both
/// packages; the referring file's own package is what picks one.
#[test]
fn a_partial_qualifier_still_reaches_the_package_rule() {
    let (code, stdout, stderr) = run(&["implements", "p1.Outer.Inner", &fixture("partial")]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("class Sub"),
        "the qualifier ended resolution instead of narrowing it:\n{stdout}\n{stderr}"
    );

    // The other package's identically-named type has no subtypes.
    let (_, stdout, _) = run(&["implements", "p2.Outer.Inner", &fixture("partial")]);
    assert!(stdout.contains("0 match(es)"), "{stdout}");
}

/// The rename guard errs toward extracting. A false positive costs one
/// reparse; a false negative deletes an edge and reports nothing — which is
/// what a scan anchored to lines *starting* an import did to `pub use` and
/// to any rename a formatter had wrapped onto its own line.
#[test]
fn the_rename_guard_sees_wrapped_and_visibility_qualified_forms() {
    let dir = format!("{}/rust", fixture("rename"));
    let (code, stdout, _) = run(&["implements", "Base", &dir]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("Child"),
        "`pub use … as …` was skipped:\n{stdout}"
    );
    assert!(
        stdout.contains("Wrap"),
        "a rename wrapped onto its own line was skipped:\n{stdout}"
    );
}

/// An edge never crosses a language, and a fixture that holds one language
/// per directory cannot see whether it does. Walking the whole fixture tree
/// is the scope this suite was blind to: a Java `extends com.acme.Widget`
/// took a TypeScript `Widget` as its supertype, and the round-3 rule that
/// spared an unevaluable qualifier let three cross-language declarations
/// survive into the ambiguity note.
#[test]
fn an_edge_never_crosses_a_language() {
    let (code, stdout, _) = run(&["implements", "Widget", &fixture("mixed")]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("class Child"),
        "the TypeScript subtype went missing:\n{stdout}"
    );
    assert!(
        !stdout.contains("class Sub"),
        "a Java class became a subtype of a TypeScript interface:\n{stdout}"
    );

    // The whole tree at once: every per-language answer has to survive being
    // walked next to the others, and nothing may be reported as ambiguous.
    let (code, stdout, stderr) = run(&["implements", "Base", &fixture("")]);
    assert_eq!(code, Some(0));
    for lang in ["csharp", "python", "typescript", "kotlin", "rust"] {
        assert!(
            stdout.contains(&format!("alias_qualifier/{lang}"))
                || stdout.contains(&format!("rename/{lang}")),
            "{lang} lost its edge in a multi-language walk:\n{stdout}"
        );
    }
    assert!(
        !stderr.contains("could be"),
        "a cross-language name collision was reported as ambiguity:\n{stderr}"
    );
}

/// A file with no package clause is still a file in a language that has
/// package clauses. Keying the qualifier's escape hatch on the *string*
/// being empty let a default-package Java interface capture a reference
/// that spelled out `java.lang`.
#[test]
fn a_default_package_file_is_not_a_namespaceless_language() {
    let (code, stdout, _) = run(&["implements", "Runnable", &fixture("default_package")]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("0 match(es)"),
        "a default-package declaration captured a qualified reference:\n{stdout}"
    );
}

/// The remaining shapes an import can take between a reference and its
/// declaration: an aliased module used as a qualifier where the name
/// collides, a NodeNext specifier naming the emitted extension, and an
/// out-of-walk target reached through a rename.
#[test]
fn an_import_still_decides_in_its_awkward_spellings() {
    let (code, stdout, stderr) = run(&[
        "implements",
        "Root",
        &format!("{}/python_collision", fixture("alias_qualifier")),
    ]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("class Child"),
        "an expanded module alias did not pick between two declarations:\n{stdout}\n{stderr}"
    );

    let (code, stdout, _) = run(&["implements", "Base", &fixture("nodenext")]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("class Child"),
        "a `./base.js` specifier did not reach `base.ts`:\n{stdout}"
    );

    let (code, stdout, _) = run(&["implements", "com.api.Base", &fixture("external_rename")]);
    assert_eq!(code, Some(0), "an out-of-walk target through a rename");
    assert!(
        stdout.contains("class Child"),
        "the rename hid an out-of-walk implementor:\n{stdout}"
    );
}

/// Spellings that agree collapse however they sort. Comparing only
/// neighbours reported three claims where the outer two were one.
#[test]
fn agreeing_spellings_collapse_out_of_order() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    for (name, base) in [
        ("A", "p.Runnable"),
        ("B", "q.Runnable"),
        ("C", "z.p.Runnable"),
    ] {
        std::fs::write(
            dir.join(format!("{name}.java")),
            format!("package app;\npublic class {name} implements {base} {{}}\n"),
        )
        .expect("write");
    }
    let (code, _, stderr) = run(&["implements", "Runnable", dir.to_str().expect("utf8")]);
    assert_eq!(code, Some(0));
    // `p.Runnable` and `z.p.Runnable` are one claim spelled at two depths;
    // sorting puts `q.Runnable` between them.
    assert!(
        stderr.contains("written 2 different ways"),
        "agreeing spellings did not collapse across a sort gap:\n{stderr}"
    );
}

/// A rename is scoped, and the scope is recoverable from the line it was
/// written on: a Rust `use … as …` inside `mod a` is not in scope in `mod
/// b`, and a C# file-scope alias loses to a type the enclosing namespace
/// declares — Kotlin and Rust read that precedence the other way, so it is
/// one language's rule rather than a general one.
#[test]
fn a_rename_reaches_only_its_own_scope() {
    for lang in ["csharp", "rust"] {
        let dir = format!("{}/{lang}", fixture("scope"));
        let (code, stdout, _) = run(&["implements", "Target", &dir]);
        assert_eq!(code, Some(0), "{lang}");
        assert!(
            stdout.contains("AChild"),
            "{lang}: the rename did not reach its own scope:\n{stdout}"
        );
        assert!(
            !stdout.contains("BChild"),
            "{lang}: the rename leaked into a sibling scope:\n{stdout}"
        );
    }

    // The sibling's own declaration is what its subtype means.
    let (_, stdout, _) = run(&[
        "implements",
        "B.Alias",
        &format!("{}/csharp", fixture("scope")),
    ]);
    assert!(stdout.contains("BChild"), "{stdout}");
    let (_, stdout, _) = run(&["implements", "Alias", &format!("{}/rust", fixture("scope"))]);
    assert!(stdout.contains("BChild"), "{stdout}");
}

/// The merged-spellings note has to see a spelling the clause reached
/// through a rename, or two distinct out-of-walk types merge in silence —
/// the clause itself says only `A`.
#[test]
fn merged_spellings_include_the_ones_a_rename_hid() {
    let (code, stdout, stderr) = run(&["implements", "External", &fixture("external_rename")]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("class One") && stdout.contains("class Two"),
        "{stdout}"
    );
    assert!(
        stderr.contains("written 2 different ways")
            && stderr.contains("a.External")
            && stderr.contains("b.External"),
        "two aliased external types merged with no note:\n{stderr}"
    );
}

/// Every import lookup is scoped, not the two that remembered to ask.
/// Sibling Rust modules can bind the same alias to different modules, and
/// an out-of-walk rename in one module is not in scope in the next.
#[test]
fn every_import_lookup_is_scoped() {
    let dir = format!("{}/rust_qualifier", fixture("scope"));
    let (_, stdout, _) = run(&["implements", "one.Base", &dir]);
    assert!(
        stdout.contains("AChild") && !stdout.contains("BChild"),
        "a sibling module's alias expanded this qualifier:\n{stdout}"
    );
    let (_, stdout, _) = run(&["implements", "two.Base", &dir]);
    assert!(
        stdout.contains("BChild") && !stdout.contains("AChild"),
        "a sibling module's alias expanded this qualifier:\n{stdout}"
    );

    // The out-of-walk path asks the same question.
    let (_, stdout, _) = run(&[
        "implements",
        "external.Target",
        &format!("{}/rust", fixture("scope")),
    ]);
    assert!(
        stdout.contains("AOutside") && !stdout.contains("BChild"),
        "an out-of-walk rename was taken outside its scope:\n{stdout}"
    );
}

/// A relative alias knows the one file it lands on, and that is stronger
/// than the segment it happens to end with: `from . import base as b`
/// names the sibling `base.py`, not every `base.py` in the tree.
#[test]
fn a_relative_module_alias_names_one_file() {
    let (code, stdout, stderr) = run(&[
        "implements",
        "Relative",
        &format!("{}/python_relative", fixture("scope")),
    ]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("class Child"),
        "the relative alias was left ambiguous:\n{stdout}\n{stderr}"
    );
    assert!(
        !stderr.contains("could be"),
        "the edge was dropped rather than resolved:\n{stderr}"
    );
}

/// C# searches the whole enclosing namespace chain before its aliases, so
/// a type in an ancestor namespace outranks a file-scope alias too.
#[test]
fn csharp_prefers_an_ancestor_namespace_over_an_alias() {
    let dir = format!("{}/csharp_nested", fixture("scope"));
    let (_, stdout, _) = run(&["implements", "X.Real.Target", &dir]);
    assert!(
        stdout.contains("0 match(es)"),
        "the file-scope alias beat an ancestor namespace's type:\n{stdout}"
    );
    let (_, stdout, _) = run(&["implements", "A.Alias", &dir]);
    assert!(stdout.contains("Child"), "{stdout}");
}

/// A nested type can be written several ways, and each has to reach the
/// declaration it names rather than the nearest same-shaped nesting.
///
/// The forms are the ones OpenRewrite enumerated for the same problem in
/// its Kotlin type attribution (openrewrite/rewrite#8238): fully qualified,
/// relative to an imported outer type, through a rename, and relative to an
/// imported *nested* type. The fixture declares one nesting path twice, in
/// `org.example` and `org.other`, so nothing but the import separates them.
#[test]
fn a_nested_type_resolves_however_the_import_shortened_it() {
    let dir = fixture("nested");
    let subtypes = |target: &str, dir: &str| -> Vec<String> {
        let (code, stdout, stderr) = run(&["implements", target, dir]);
        assert_eq!(code, Some(0), "{target}: {stderr}");
        let mut names: Vec<String> = stdout
            .lines()
            .skip(1)
            .filter_map(|l| l.split_whitespace().nth(2).map(str::to_string))
            .collect();
        names.sort();
        names
    };

    // Java: the outer type imported, and one level in imported.
    let java = format!("{dir}/java");
    assert_eq!(
        subtypes("org.example.TestClass.NestedClass.SubNestedClass", &java),
        ["ChildNested", "ChildOuter"],
        "an imported outer or nested type did not pin the reference"
    );
    // The identically-shaped path in the other package keeps none of them.
    assert!(subtypes("org.other.TestClass.NestedClass.SubNestedClass", &java).is_empty());

    // Kotlin: the same two forms plus a rename, against their own pair.
    let kt = format!("{dir}/kotlin");
    assert_eq!(
        subtypes("org.example.TestClass.NestedClass.SubNestedClass", &kt),
        ["ChildAliased", "ChildOuter"],
        "a renamed nested import did not pin the reference"
    );
    assert!(subtypes("org.other.TestClass.NestedClass.SubNestedClass", &kt).is_empty());

    // An imported nested type whose leaf repeats deeper: `B.A.C` is a
    // trailing run of `A.B.A.C`, and the bare receiver `A` must still mean
    // the outer type rather than the one repeating inside it.
    assert_eq!(subtypes("foo.bar.A.B.A.C", &dir), ["Deep"]);
    assert_eq!(subtypes("foo.bar.A", &dir), ["Shallow"]);

    // The same simple name under two outers in one package.
    assert_eq!(subtypes("org.two.Outer1.Item", &dir), ["PickOne"]);
    assert_eq!(subtypes("org.two.Outer2.Item", &dir), ["PickTwo"]);

    // None of this is reached by guessing: nothing is reported as dropped.
    let (_, _, stderr) = run(&["implements", "SubNestedClass", &dir]);
    assert!(
        !stderr.contains("could be"),
        "an import that pinned the reference went unused:\n{stderr}"
    );
}

/// A namespace can be several segments in one declaration — PHP writes
/// `namespace App\Base`, C++17 writes `namespace A::B` — and a reference to
/// it splits on those separators. The declaration's own qualified name did
/// not, so the two sides never agreed: the qualifier rejected every
/// candidate, the edge vanished, and it was not even reported as dropped,
/// since a rule that rejects everything leaves nothing to be ambiguous
/// about. That is essentially every PSR-4 project.
#[test]
fn a_namespace_written_in_one_declaration_still_has_segments() {
    let php = format!("{}/php", fixture("namespace_separators"));
    // The reference spells `\App\Base\Root`, and the two spellings of the
    // target have to agree with each other as well as with it.
    for target in ["App\\Base\\Root", "App.Base.Root"] {
        let (code, stdout, stderr) = run(&["implements", target, &php]);
        assert_eq!(code, Some(0), "{target}: {stderr}");
        assert!(
            stdout.contains("class Child"),
            "{target}: a multi-segment namespace lost its edge:\n{stdout}"
        );
    }
    // And the qualifier still discriminates, which is what says the
    // segments are compared rather than ignored.
    let (code, stdout, _) = run(&["implements", "App\\Other\\Root", &php]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("0 match(es)"),
        "the sibling namespace claimed the reference:\n{stdout}"
    );

    let cpp = format!("{}/cpp", fixture("namespace_separators"));
    let (code, stdout, stderr) = run(&["implements", "A::B::Root", &cpp]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains("class Child"),
        "a C++17 nested namespace lost its edge:\n{stdout}"
    );
    let (_, stdout, _) = run(&["implements", "A::C::Root", &cpp]);
    assert!(
        stdout.contains("0 match(es)"),
        "the sibling namespace claimed the reference:\n{stdout}"
    );
}

/// A name the file rebinds is bound whether or not the walk holds what it
/// was bound to. An external dependency normally is *not* in the walk, so
/// treating "the rename resolved to nothing" as "there was no rename" sent
/// the alias on to whatever unrelated declaration carried the same simple
/// name — the silent merge this resolver exists to stop, in the case where
/// it is most likely to happen.
///
/// Two shapes rebind: the type itself (`import com.external.Thing as
/// Widget`) and the head of a qualifier (`use external::stuff as api`,
/// making `api::Handle` mean `external::stuff::Handle`). Both are checked
/// in both directions, because suppressing an edge under the wrong name is
/// only half an answer if the right name cannot find it either.
#[test]
fn a_rebinding_the_walk_cannot_follow_still_claims_the_name() {
    let kt = format!("{}/kotlin", fixture("unresolved_rename"));
    for target in ["Widget", "api.Widget"] {
        let (code, stdout, stderr) = run(&["implements", target, &kt]);
        assert_eq!(code, Some(0), "{target}: {stderr}");
        assert!(
            stdout.contains("0 match(es)"),
            "{target}: an alias bound to an out-of-walk type claimed a local declaration:\n{stdout}"
        );
    }
    let (code, stdout, _) = run(&["implements", "com.external.Thing", &kt]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("class Child"),
        "the alias's real target lost its implementor:\n{stdout}"
    );

    let rs = format!("{}/rust", fixture("unresolved_rename"));
    let (code, stdout, stderr) = run(&["implements", "Handle", &rs]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains("0 match(es)"),
        "an aliased qualifier claimed a local declaration:\n{stdout}"
    );
    let (code, stdout, _) = run(&["implements", "external.stuff.Handle", &rs]);
    assert_eq!(code, Some(0), "the aliased qualifier's real target");
    assert!(
        stdout.contains("struct Child"),
        "the aliased qualifier's real target lost its implementor:\n{stdout}"
    );
    // The alias names one module, not any module ending in that segment.
    let (code, _, _) = run(&["implements", "other.stuff.Handle", &rs]);
    assert_eq!(
        code,
        Some(2),
        "an unrelated qualifier was answered for by the alias"
    );
}
