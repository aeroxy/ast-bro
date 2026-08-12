//! Coverage of the hierarchy, not just of the match list.
//!
//! `implements` resolves a chain by matching one walked type's base against
//! another walked type's name, so a link declared outside the walked path(s)
//! ends the chain and takes every subtype below it with it. That answer used
//! to be a bare number: `implements BinaryCodec .../jdbc/codec/` reported 7
//! of the 52 subtypes living in that very directory, at exit 0, with nothing
//! on stderr.
//!
//! The fixture is that shape in miniature — `api/` declares the interfaces,
//! `codec/` the classes that inherit through them — plus the two kinds of
//! unresolved base that must *not* raise a note: a library type the project
//! never declares, and a project type that leads nowhere near the target.

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

/// A four-level hierarchy split across two directories, plus the three
/// shapes that must raise nothing.
///
/// `Base` ← `Middle` ← `Deep` (all in api) ← the leaves in codec, with
/// `LeafTwo` inheriting `Base` directly so a walk of `codec/` alone still
/// has one match to report. `Deep` is what a single probe round cannot
/// settle: proving it inherits `Base` means reading `Deep.java`, then
/// `Middle.java`, which only the round after names.
///
/// The quiet shapes: `Serializable`, which the project never declares;
/// `Unrelated`, declared in api with an ancestry of its own that ends in a
/// library type; and `IllegalStateException`, a project class carrying the
/// simple name of the library type `Boom` actually extends.
///
/// The `.git` directory is the project-root marker the probe walks up to:
/// without it the search would climb past the temporary directory.
fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::create_dir(root.join("api")).unwrap();
    std::fs::create_dir(root.join("codec")).unwrap();
    let write = |rel: &str, src: &str| std::fs::write(root.join(rel), src).unwrap();
    write("api/Base.java", "public interface Base {}\n");
    write("api/Middle.java", "public interface Middle extends Base {}\n");
    write("api/Deep.java", "public interface Deep extends Middle {}\n");
    write(
        "api/Unrelated.java",
        "public interface Unrelated extends java.io.Serializable {}\n",
    );
    write(
        "api/IllegalStateException.java",
        "public class IllegalStateException extends Unrelated {}\n",
    );
    write("codec/LeafOne.java", "public class LeafOne implements Middle {}\n");
    write(
        "codec/LeafTwo.java",
        "public class LeafTwo implements Base, java.io.Serializable {}\n",
    );
    write(
        "codec/LeafThree.java",
        "public class LeafThree implements Middle {}\n",
    );
    write("codec/LeafFour.java", "public class LeafFour implements Deep {}\n");
    write("codec/Other.java", "public class Other implements Unrelated {}\n");
    write(
        "codec/Boom.java",
        "public class Boom extends java.lang.IllegalStateException {}\n",
    );
    dir
}

/// A walk that covers the whole hierarchy reports every subtype and adds no
/// qualification.
///
/// This is the control: the same fixture, the same target, and the count the
/// scoped walk below is measured against. If it drops to three, the fixture
/// stopped exercising transitive resolution rather than the note regressing.
#[test]
fn whole_project_walk_finds_every_subtype_and_stays_quiet() {
    let dir = project();
    let root = dir.path().to_str().unwrap();
    let (code, stdout, stderr) = run(&["implements", "Base", root]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    // Middle, Deep and the four leaves. Counted by hand from the fixture:
    // Other and Boom hang off the quiet shapes, and IllegalStateException
    // inherits Unrelated, so none of the three is a Base.
    assert!(stdout.contains("6 match(es)"), "stdout:\n{stdout}");
    assert!(
        !stderr.contains("# note:"),
        "a complete walk must not be qualified:\n{stderr}"
    );
}

/// A walk that cannot see `Middle` says so, names the file that declares it,
/// and still exits 0.
///
/// The three subtypes reachable only through `Middle` are absent from the
/// answer, which is the loss this file is about: without the note, `1
/// match(es)` reads exactly like a complete answer.
#[test]
fn scoped_walk_reports_the_ancestors_it_could_not_read() {
    let dir = project();
    let scope = dir.path().join("codec");
    let (code, stdout, stderr) = run(&["implements", "Base", scope.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(stdout.contains("1 match(es)"), "stdout:\n{stdout}");
    assert!(
        stderr.contains("# note:") && stderr.contains("2 base type(s)"),
        "expected the hierarchy note on stderr:\n{stderr}"
    );
    for named in ["Middle", "Deep", "api/Middle.java", "api/Deep.java"] {
        assert!(stderr.contains(named), "note must name {named}:\n{stderr}");
    }
}

/// The target's own declaration sitting outside the walk is not a gap.
///
/// `implements` seeds direct matches from base-name equality and never reads
/// the target's declaration, so a walk holding the implementations but not
/// the interface — the ordinary api/impl split — is already complete.
/// Qualifying it would fire on the most common layout there is.
#[test]
fn the_targets_own_declaration_is_not_a_gap() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::create_dir(root.join("api")).unwrap();
    std::fs::create_dir(root.join("codec")).unwrap();
    std::fs::write(root.join("api/Base.java"), "public interface Base {}\n").unwrap();
    std::fs::write(
        root.join("codec/Leaf.java"),
        "public class Leaf implements Base {}\n",
    )
    .unwrap();

    let scope = root.join("codec");
    let (code, stdout, stderr) = run(&[
        "implements",
        "Base",
        scope.to_str().unwrap(),
        "--json",
        "--compact",
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(doc["matches"].as_array().unwrap().len(), 1);
    assert_eq!(doc["hierarchy_truncated"], false, "json:\n{stdout}");
    assert!(!stderr.contains("# note:"), "stderr:\n{stderr}");

    let (_, text, stderr) = run(&["implements", "Base", scope.to_str().unwrap()]);
    assert!(text.contains("1 match(es)"), "stdout:\n{text}");
    assert!(!stderr.contains("# note:"), "stderr:\n{stderr}");
}

/// A link the walk never names is still read, so the base above it is
/// proved.
///
/// The walk names `Mid2` and nothing else about the chain: `Mid1` is named
/// only by `Mid2.java`, which the probe has to read before it can even ask
/// about `Mid1`. A probe that stopped after one round would leave `Mid2`
/// unproved and drop it, and this is the fixture that forces the second
/// round — in the shared `project()` fixture the walk happens to name every
/// link itself, so one round settles it.
#[test]
fn a_link_named_only_outside_the_walk_is_followed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::create_dir(root.join("api")).unwrap();
    std::fs::create_dir(root.join("codec")).unwrap();
    let write = |rel: &str, src: &str| std::fs::write(root.join(rel), src).unwrap();
    write("api/Base.java", "public interface Base {}\n");
    write("api/Mid1.java", "public interface Mid1 extends Base {}\n");
    write("api/Mid2.java", "public interface Mid2 extends Mid1 {}\n");
    write("codec/Leaf.java", "public class Leaf implements Mid2 {}\n");
    write("codec/Direct.java", "public class Direct implements Base {}\n");

    let scope = root.join("codec");
    let (code, stdout, stderr) = run(&["implements", "Base", scope.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(stdout.contains("1 match(es)"), "stdout:\n{stdout}");
    assert!(
        stderr.contains("Mid2") && stderr.contains("api/Mid2.java"),
        "a chain proved only through an unnamed link must still be reported:\n{stderr}"
    );
}

/// The note lists only bases that can still hide a subtype of the target.
///
/// `Serializable` is a library type no walk could resolve. `Unrelated` is a
/// project type whose own ancestry ends in one, which is the shape that
/// makes "not yet ruled out" useless as a criterion — in a language whose
/// types bottom out in library interfaces, nearly every base has it.
/// `IllegalStateException` is a project class sharing the simple name of the
/// library type `Boom` extends; reporting it would send the reader to a file
/// that has nothing to do with the query.
#[test]
fn library_and_off_hierarchy_bases_are_not_reported() {
    let dir = project();
    let scope = dir.path().join("codec");
    let (_, _, stderr) = run(&["implements", "Base", scope.to_str().unwrap()]);
    for quiet in ["Serializable", "Unrelated", "IllegalStateException"] {
        assert!(
            !stderr.contains(quiet),
            "{quiet} cannot hide a subtype of Base:\n{stderr}"
        );
    }
}

/// A note is raised for a walk spelled as a glob, not only for one spelled
/// as a directory.
///
/// `implements` takes any path the walker takes, and a glob reaches the
/// walker unexpanded — so a probe that looked for the project root at the
/// pattern itself found nothing on disk and went silent on exactly the
/// narrow walks it is for.
#[test]
fn a_globbed_walk_is_qualified_too() {
    let dir = project();
    let glob = dir.path().join("codec").join("*.java");
    let (code, stdout, stderr) = run(&["implements", "Base", glob.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(stdout.contains("1 match(es)"), "stdout:\n{stdout}");
    assert!(
        stderr.contains("# note:") && stderr.contains("api/Middle.java"),
        "expected the hierarchy note on stderr:\n{stderr}"
    );
}

/// The probe reads every language it can parse, not only the ones the walk
/// happened to contain.
///
/// A Scala class inheriting a Java interface is one project, and a walk of
/// the Scala directory alone must still find the Java declaration that
/// carries the rest of the chain.
#[test]
fn the_probe_crosses_languages() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::create_dir(root.join("api")).unwrap();
    std::fs::create_dir(root.join("scala")).unwrap();
    std::fs::write(root.join("api/Base.java"), "public interface Base {}\n").unwrap();
    std::fs::write(
        root.join("api/Middle.java"),
        "public interface Middle extends Base {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("scala/Leaf.scala"),
        "class Leaf extends Middle\n",
    )
    .unwrap();
    // One direct implementor inside the scope, so the answer is a qualified
    // result rather than the exit-2 rejection a target absent from the walk
    // earns.
    std::fs::write(root.join("scala/Direct.scala"), "class Direct extends Base\n").unwrap();

    let scope = root.join("scala");
    let (code, stdout, stderr) = run(&["implements", "Base", scope.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(stdout.contains("1 match(es)"), "stdout:\n{stdout}");
    assert!(
        stderr.contains("# note:") && stderr.contains("api/Middle.java"),
        "a .java link must be found from a .scala walk:\n{stderr}"
    );
}

/// The note spells its paths the way the answer beside it does.
///
/// The note's advice is "add those paths to the walk", so its paths have to
/// hang off the directory the caller named — including when that spelling
/// reaches the project through a symlink, where the root lookup canonicalizes
/// and would otherwise answer `/private/tmp/…` for a `/tmp/…` walk.
#[test]
fn note_paths_follow_the_spelling_of_the_walk() {
    let dir = project();
    let link = dir.path().parent().unwrap().join(format!(
        "{}-link",
        dir.path().file_name().unwrap().to_str().unwrap()
    ));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(dir.path(), &link).unwrap();

    let scope = link.join("codec");
    let (_, stdout, stderr) = run(&["implements", "Base", scope.to_str().unwrap()]);
    let prefix = link.to_str().unwrap();
    assert!(
        stdout.contains(prefix),
        "the walk's own spelling is on stdout:\n{stdout}"
    );
    let listed: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("#   "))
        .filter_map(|l| l.split_whitespace().nth(2))
        .collect();
    assert!(!listed.is_empty(), "no bases listed:\n{stderr}");
    for path in listed {
        assert!(
            path.starts_with(prefix),
            "note path {path} must hang off the walk's own {prefix}:\n{stderr}"
        );
        assert!(
            std::path::Path::new(path).exists(),
            "note path {path} must name a file the reader can add:\n{stderr}"
        );
    }
    std::fs::remove_file(&link).unwrap();
}

/// `--json` carries the same qualification as `hierarchy_truncated` plus the
/// bases behind it, so a consumer that never reads stderr still sees it.
#[test]
fn json_carries_hierarchy_truncated_and_the_unseen_bases() {
    let dir = project();
    let scope = dir.path().join("codec");
    let (code, stdout, stderr) = run(&[
        "implements",
        "Base",
        scope.to_str().unwrap(),
        "--json",
        "--compact",
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(doc["schema"], "ast-bro.implements.v1");
    assert_eq!(doc["hierarchy_truncated"], true);
    let names: Vec<&str> = doc["unseen_bases"]
        .as_array()
        .expect("unseen_bases array")
        .iter()
        .map(|b| b["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, ["Deep", "Middle"], "json:\n{stdout}");
    assert!(
        doc["unseen_bases"][0]["path"]
            .as_str()
            .expect("path")
            .ends_with("api/Deep.java"),
        "json:\n{stdout}"
    );
}

/// A base the probe could not place is named in `unlocated_bases`, so
/// `hierarchy_truncated: false` never has to carry a claim the search cannot
/// back.
///
/// `Middle` here is declared in a file named after neither it nor anything
/// else the probe asks for, which is the shape a file-name lookup cannot
/// find: an inner type, a secondary type, a Python module holding several
/// classes. Silence about it would read as "no gap", and there is one —
/// `implements Base .` finds `Leaf` through exactly this link.
#[test]
fn a_base_the_probe_cannot_place_is_named_as_unplaced() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::create_dir(root.join("api")).unwrap();
    std::fs::create_dir(root.join("codec")).unwrap();
    std::fs::write(root.join("api/Base.java"), "public interface Base {}\n").unwrap();
    // Two types in one file: `Middle` cannot be found by file name.
    std::fs::write(
        root.join("api/Types.java"),
        "interface Middle extends Base {}\ninterface Types {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("codec/Leaf.java"),
        "public class Leaf implements Middle {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("codec/Direct.java"),
        "public class Direct implements Base {}\n",
    )
    .unwrap();

    let scope = root.join("codec");
    let (_, whole, _) = run(&["implements", "Base", root.to_str().unwrap()]);
    assert!(
        whole.contains("3 match(es)"),
        "the link is real, so the scoped answer below is short:\n{whole}"
    );

    let (code, stdout, stderr) = run(&[
        "implements",
        "Base",
        scope.to_str().unwrap(),
        "--json",
        "--compact",
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    let unlocated: Vec<&str> = doc["unlocated_bases"]
        .as_array()
        .expect("unlocated_bases array")
        .iter()
        .map(|n| n.as_str().expect("name"))
        .collect();
    assert!(
        unlocated.contains(&"Middle"),
        "a base the probe could not place must be named:\n{stdout}"
    );
}

/// A zero says on stderr that the search could not place every name, so the
/// default surface is not silent about a gap it cannot rule out.
///
/// `Middle` is declared in a file named after neither it nor anything the
/// probe asks for, and a bare `0 match(es)` beside that would read as "no
/// such subtype" — while `implements Base .` finds one. The line stays off
/// non-empty answers: there the unplaced names are the library surface, and
/// the count belongs in `unlocated_bases` instead.
#[test]
fn an_empty_answer_says_the_search_could_not_place_a_name() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::create_dir(root.join("api")).unwrap();
    std::fs::create_dir(root.join("codec")).unwrap();
    std::fs::write(root.join("codec/Base.java"), "public interface Base {}\n").unwrap();
    std::fs::write(
        root.join("api/Types.java"),
        "interface Middle extends Base {}\ninterface Types {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("codec/Leaf.java"),
        "public class Leaf implements Middle {}\n",
    )
    .unwrap();

    let scope = root.join("codec");
    let (code, stdout, stderr) = run(&["implements", "Base", scope.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(stdout.contains("0 match(es)"), "stdout:\n{stdout}");
    assert!(
        stderr.contains("# note:") && stderr.contains("not proof of none"),
        "a zero must say the search could not place every name:\n{stderr}"
    );
    // The count qualifies the zero; the names stay in `--json`, where a
    // library type in the list costs the reader nothing.
    assert!(
        !stderr.contains("Middle"),
        "the note names no types:\n{stderr}"
    );
    let (_, json, _) = run(&[
        "implements",
        "Base",
        scope.to_str().unwrap(),
        "--json",
        "--compact",
    ]);
    assert!(
        json.contains("\"unlocated_bases\":[\"Middle\"]"),
        "json must carry the name:\n{json}"
    );

    // The same fixture with a match to show keeps the line off stderr: the
    // answer itself is the signal there, and `unlocated_bases` carries the
    // rest for a consumer that wants it.
    std::fs::write(
        root.join("codec/Direct.java"),
        "public class Direct implements Base {}\n",
    )
    .unwrap();
    let (_, stdout, stderr) = run(&["implements", "Base", scope.to_str().unwrap()]);
    assert!(stdout.contains("1 match(es)"), "stdout:\n{stdout}");
    assert!(
        !stderr.contains("# note:"),
        "an answer with a match carries no unplaced-name line:\n{stderr}"
    );
}

/// A TypeScript declaration file is found under the type it declares.
///
/// `Middle.d.ts` has the file stem `Middle.d`, which matches no type name —
/// and an interface a scoped walk inherits is exactly what a `.d.ts` holds.
#[test]
fn a_declaration_file_is_found_under_its_type() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::create_dir(root.join("api")).unwrap();
    std::fs::create_dir(root.join("codec")).unwrap();
    let write = |rel: &str, src: &str| std::fs::write(root.join(rel), src).unwrap();
    write("api/base.ts", "export interface Base {}\n");
    write(
        "api/Middle.d.ts",
        "export interface Middle extends Base {}\n",
    );
    write("codec/leaf.ts", "export class Leaf implements Middle {}\n");
    write("codec/direct.ts", "export class Direct implements Base {}\n");

    let scope = root.join("codec");
    let (code, stdout, stderr) = run(&["implements", "Base", scope.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(stdout.contains("1 match(es)"), "stdout:\n{stdout}");
    assert!(
        stderr.contains("api/Middle.d.ts"),
        "a .d.ts link must be found:\n{stderr}"
    );
}

/// A walk spelled relatively from inside the project answers relatively.
///
/// `cd module && implements Base codec` is the ordinary multi-module habit,
/// and it is the case where recovering the caller's spelling by dropping
/// components runs out of path: the note used to answer it with the
/// canonical absolute root beside relative results.
#[test]
fn a_walk_from_a_subdirectory_keeps_relative_note_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::create_dir_all(root.join("module/api")).unwrap();
    std::fs::create_dir(root.join("module/codec")).unwrap();
    std::fs::write(
        root.join("module/api/Base.java"),
        "public interface Base {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("module/api/Middle.java"),
        "public interface Middle extends Base {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("module/codec/Leaf.java"),
        "public class Leaf implements Middle {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("module/codec/Direct.java"),
        "public class Direct implements Base {}\n",
    )
    .unwrap();

    let out = std::process::Command::new(bin())
        .args(["implements", "Base", "codec"])
        .current_dir(root.join("module"))
        .env("NO_COLOR", "1")
        .output()
        .expect("run ast-bro");
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    let stderr = String::from_utf8(out.stderr).expect("utf8");
    assert!(
        stdout.contains("codec/Direct.java"),
        "results are relative here:\n{stdout}"
    );
    let listed: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("#   "))
        .filter_map(|l| l.split_whitespace().nth(2))
        .collect();
    assert_eq!(
        listed,
        ["api/Middle.java"],
        "note paths must be spelled like the results:\n{stderr}"
    );
}

/// A relative walk answers relatively however the working directory and the
/// project root are arranged.
///
/// The three shapes are: inside the project (covered above), above it, and
/// beside it in a sibling checkout. The last two are what a prefix recovered
/// by dropping components cannot express — there is nothing to drop — and
/// where the note used to fall back to the canonical absolute root while the
/// results stayed relative.
#[test]
fn a_relative_walk_from_outside_the_project_still_answers_relatively() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for repo in ["proj", "other"] {
        std::fs::create_dir_all(root.join(repo).join(".git")).unwrap();
        std::fs::create_dir(root.join(repo).join("api")).unwrap();
        std::fs::create_dir(root.join(repo).join("codec")).unwrap();
    }
    let write = |rel: &str, src: &str| std::fs::write(root.join(rel), src).unwrap();
    write("proj/api/Base.java", "public interface Base {}\n");
    write(
        "proj/api/Middle.java",
        "public interface Middle extends Base {}\n",
    );
    write(
        "proj/codec/Leaf.java",
        "public class Leaf implements Middle {}\n",
    );
    write(
        "proj/codec/Direct.java",
        "public class Direct implements Base {}\n",
    );

    // cwd above the project root.
    let out = std::process::Command::new(bin())
        .args(["implements", "Base", "proj/codec"])
        .current_dir(root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run ast-bro");
    let stderr = String::from_utf8(out.stderr).expect("utf8");
    let listed: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("#   "))
        .filter_map(|l| l.split_whitespace().nth(2))
        .collect();
    assert_eq!(
        listed,
        ["proj/api/Middle.java"],
        "a walk from above the root answers from there:\n{stderr}"
    );

    // cwd in a sibling checkout: the route out and back in.
    let out = std::process::Command::new(bin())
        .args(["implements", "Base", "../proj/codec"])
        .current_dir(root.join("other"))
        .env("NO_COLOR", "1")
        .output()
        .expect("run ast-bro");
    let stderr = String::from_utf8(out.stderr).expect("utf8");
    let listed: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("#   "))
        .filter_map(|l| l.split_whitespace().nth(2))
        .collect();
    assert_eq!(
        listed,
        ["../proj/api/Middle.java"],
        "a walk from a sibling answers through it:\n{stderr}"
    );
}

/// When two files declare the same simple name, the note points at the one
/// that is the route to the target.
///
/// Names are pooled when an ancestry is walked, so the pair
/// (`a/Middle` unrelated, `z/Middle extends Base`) proves the *name* reaches
/// `Base` while the first file does not. Naming `a/Middle.java` would send
/// the reader to add a path that restores nothing.
#[test]
fn the_note_names_the_declaration_that_reaches_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::create_dir(root.join("a")).unwrap();
    std::fs::create_dir(root.join("z")).unwrap();
    std::fs::create_dir(root.join("codec")).unwrap();
    let write = |rel: &str, src: &str| std::fs::write(root.join(rel), src).unwrap();
    write("z/Base.java", "public interface Base {}\n");
    write("a/Middle.java", "public interface Middle {}\n");
    write("z/Middle.java", "public interface Middle extends Base {}\n");
    write("codec/Leaf.java", "public class Leaf implements Middle {}\n");
    write("codec/Direct.java", "public class Direct implements Base {}\n");

    let scope = root.join("codec");
    let (code, _, stderr) = run(&["implements", "Base", scope.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let listed: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("#   "))
        .filter_map(|l| l.split_whitespace().nth(2))
        .collect();
    assert_eq!(listed.len(), 1, "one base is a route here:\n{stderr}");
    assert!(
        listed[0].ends_with("z/Middle.java"),
        "the note must name the declaration that inherits Base:\n{stderr}"
    );
}

/// A declaration is found through the word separators its language uses.
///
/// `StreamingBinaryCodec` lives in `streaming_binary_codec.py` in Python and
/// in `StreamingBinaryCodec.java` in Java, and both are the same file-name
/// convention as far as the probe is concerned.
#[test]
fn a_snake_case_file_name_is_matched_to_its_type() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::create_dir(root.join("api")).unwrap();
    std::fs::create_dir(root.join("codec")).unwrap();
    std::fs::write(root.join("api/base.py"), "class Base: pass\n").unwrap();
    std::fs::write(
        root.join("api/streaming_binary_codec.py"),
        "class StreamingBinaryCodec(Base): pass\n",
    )
    .unwrap();
    std::fs::write(
        root.join("codec/leaf.py"),
        "class Leaf(StreamingBinaryCodec): pass\n",
    )
    .unwrap();
    std::fs::write(root.join("codec/direct.py"), "class Direct(Base): pass\n").unwrap();

    let scope = root.join("codec");
    let (code, _, stderr) = run(&["implements", "Base", scope.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stderr.contains("StreamingBinaryCodec") && stderr.contains("streaming_binary_codec.py"),
        "a snake_case file must be matched to its type:\n{stderr}"
    );
}

/// A walk that spans two repositories is probed in both.
///
/// Each root is its own project, and a link missing from either is missing
/// from the answer just the same.
#[test]
fn every_project_the_walk_reached_is_probed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for repo in ["one", "two"] {
        std::fs::create_dir_all(root.join(repo).join(".git")).unwrap();
        std::fs::create_dir(root.join(repo).join("api")).unwrap();
        std::fs::create_dir(root.join(repo).join("codec")).unwrap();
    }
    std::fs::write(root.join("one/api/Base.java"), "public interface Base {}\n").unwrap();
    std::fs::write(
        root.join("one/codec/Direct.java"),
        "public class Direct implements Base {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("two/api/Middle.java"),
        "public interface Middle extends Base {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("two/codec/Leaf.java"),
        "public class Leaf implements Middle {}\n",
    )
    .unwrap();

    let one = root.join("one/codec");
    let two = root.join("two/codec");
    let (code, _, stderr) = run(&[
        "implements",
        "Base",
        one.to_str().unwrap(),
        two.to_str().unwrap(),
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(
        stderr.contains("two/api/Middle.java"),
        "the second repository must be probed too:\n{stderr}"
    );
}

/// A complete walk reports `hierarchy_truncated: false` rather than omitting
/// the field, so a consumer reads one shape whatever the answer.
#[test]
fn json_states_a_complete_hierarchy_explicitly() {
    let dir = project();
    let root = dir.path().to_str().unwrap();
    let (_, stdout, _) = run(&["implements", "Base", root, "--json", "--compact"]);
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(doc["hierarchy_truncated"], false);
    assert_eq!(doc["unseen_bases"].as_array().unwrap().len(), 0);
}

/// `--direct` is never qualified: it reports the types that name the target
/// themselves, which an ancestor outside the walk cannot add to or remove
/// from.
#[test]
fn direct_answers_carry_no_hierarchy_note() {
    let dir = project();
    let scope = dir.path().join("codec");
    let (code, stdout, stderr) = run(&[
        "implements",
        "Base",
        scope.to_str().unwrap(),
        "--direct",
        "--json",
        "--compact",
    ]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid json");
    assert_eq!(doc["hierarchy_truncated"], false);
    assert!(!stderr.contains("# note:"), "stderr:\n{stderr}");
}

/// An answer of zero is qualified too.
///
/// With `Base` inside the scope and `Middle` outside it, every subtype is
/// reachable only through a link the walk cannot read, so the answer is
/// empty. A bare `0 match(es)` there is the confidently-wrong answer the
/// error contract calls out: an honest zero and a collapsed scope must not
/// look alike.
#[test]
fn an_empty_answer_says_what_the_walk_could_not_see() {
    let dir = project();
    std::fs::remove_file(dir.path().join("codec/LeafTwo.java")).unwrap();
    std::fs::rename(
        dir.path().join("api/Base.java"),
        dir.path().join("codec/Base.java"),
    )
    .unwrap();
    let scope = dir.path().join("codec");
    let (code, stdout, stderr) = run(&["implements", "Base", scope.to_str().unwrap()]);
    assert_eq!(code, Some(0), "stderr:\n{stderr}");
    assert!(stdout.contains("0 match(es)"), "stdout:\n{stdout}");
    assert!(
        stderr.contains("# note:") && stderr.contains("Middle"),
        "expected the hierarchy note on stderr:\n{stderr}"
    );
}

/// A scope that holds neither the target nor anything naming it stays a
/// rejected call, not a qualified answer.
///
/// The walk has no evidence the type exists at all, so issue #36's exit-2
/// gate fires first and its hint is already the right advice — the note this
/// file is about qualifies a delivered answer, and there is none here.
#[test]
fn a_target_absent_from_the_scope_is_still_exit_2() {
    let dir = project();
    std::fs::remove_file(dir.path().join("codec/LeafTwo.java")).unwrap();
    let scope = dir.path().join("codec");
    let (code, stdout, stderr) = run(&["implements", "Base", scope.to_str().unwrap()]);
    assert_eq!(code, Some(2), "stderr:\n{stderr}");
    assert!(stdout.is_empty(), "stdout must be empty:\n{stdout}");
    assert!(
        stderr.contains("no type named 'Base'") && stderr.contains("widen the search path"),
        "stderr:\n{stderr}"
    );
}
