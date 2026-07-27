use std::path::{Path, PathBuf};
use std::process::Command;

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ast-bro"))
}

fn run_in(root: &Path, args: &[&str]) -> String {
    let output = Command::new(binary())
        .args(args)
        .current_dir(root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run ast-bro");
    assert!(
        output.status.success(),
        "ast-bro {args:?} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8 stdout")
}

#[test]
fn opencode_cli_install_status_uninstall_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let prompt_install = run_in(root, &["install", "--target", "opencode", "--local"]);
    assert!(prompt_install.contains("opencode"));
    assert!(prompt_install.contains("prompt"));
    assert!(std::fs::read_to_string(root.join("AGENTS.md"))
        .unwrap()
        .contains("ast-bro:begin"));

    let feature_install = run_in(
        root,
        &[
            "install", "--target", "opencode", "--local", "--mcp", "--skills",
        ],
    );
    assert!(feature_install.contains("mcp"));
    assert!(feature_install.contains("skills"));

    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("opencode.json")).unwrap())
            .unwrap();
    assert_eq!(
        config["mcp"]["ast-bro"]["command"],
        serde_json::json!(["ast-bro", "mcp"])
    );
    assert_eq!(config["mcp"]["ast-bro"]["enabled"], true);
    assert!(
        std::fs::read_to_string(root.join(".opencode/skills/ast-bro/SKILL.md"))
            .unwrap()
            .contains("name: ast-bro")
    );

    let installed_status = run_in(root, &["status", "--local"]);
    let opencode_status = installed_status
        .lines()
        .find(|line| line.starts_with("opencode"))
        .expect("OpenCode status row");
    assert!(opencode_status.contains(&format!("prompt {}", env!("CARGO_PKG_VERSION"))));
    assert!(opencode_status.contains("mcp ✓"));
    assert!(opencode_status.contains("skills ✓"));

    let uninstall = run_in(root, &["uninstall", "--target", "opencode", "--local"]);
    assert!(uninstall.contains("removed"));

    let agents = std::fs::read_to_string(root.join("AGENTS.md")).unwrap();
    assert!(!agents.contains("ast-bro:begin"));
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("opencode.json")).unwrap())
            .unwrap();
    assert!(config["mcp"].get("ast-bro").is_none());
    assert!(!root.join(".opencode/skills/ast-bro/SKILL.md").exists());

    let clean_status = run_in(root, &["status", "--local"]);
    let opencode_status = clean_status
        .lines()
        .find(|line| line.starts_with("opencode"))
        .expect("OpenCode status row");
    assert!(opencode_status.contains("prompt -"));
    assert!(opencode_status.contains("mcp -"));
    assert!(opencode_status.contains("skills -"));
}
