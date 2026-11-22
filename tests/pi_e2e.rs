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

fn pi_status(root: &Path) -> String {
  run_in(root, &["status", "--local"])
    .lines()
    .find(|line| line.starts_with("pi "))
    .expect("pi status row")
    .to_string()
}

#[test]
fn pi_cli_install_status_uninstall_lifecycle() {
  let dir = tempfile::tempdir().unwrap();
  let root = dir.path();

  let install = run_in(
    root,
    &["install", "--target", "pi", "--local", "--min-lines", "150"],
  );
  assert!(install.contains("prompt"));
  assert!(install.contains("hook"));
  assert!(std::fs::read_to_string(root.join("AGENTS.md"))
    .unwrap()
    .contains("ast-bro:begin"));
  assert!(
    std::fs::read_to_string(root.join(".pi/extensions/ast-bro.ts"))
      .unwrap()
      .contains(r#""--min-lines","150""#)
  );

  let features = run_in(
    root,
    &["install", "--target", "pi", "--local", "--mcp", "--skills"],
  );
  assert!(features.contains("mcp"));
  assert!(features.contains("skills"));
  let mcp: serde_json::Value =
    serde_json::from_str(&std::fs::read_to_string(root.join(".pi/mcp.json")).unwrap()).unwrap();
  assert_eq!(mcp["mcpServers"]["ast-bro"]["command"], "ast-bro");
  assert!(root.join(".pi/skills/ast-bro/SKILL.md").exists());

  let status = pi_status(root);
  assert!(status.contains(&format!("prompt {}", env!("CARGO_PKG_VERSION"))));
  assert!(status.contains("hook ✓"));
  assert!(status.contains("mcp ✓"));
  assert!(status.contains("skills ✓"));

  let uninstall = run_in(root, &["uninstall", "--target", "pi", "--local"]);
  assert!(uninstall.contains("removed"));
  assert!(!root.join(".pi/extensions/ast-bro.ts").exists());
  assert!(!root.join(".pi/skills/ast-bro/SKILL.md").exists());

  let status = pi_status(root);
  assert!(status.contains("prompt -"));
  assert!(status.contains("hook -"));
  assert!(status.contains("mcp -"));
  assert!(status.contains("skills -"));
}

// `dirs::home_dir` ignores `HOME` on Windows, so the home directory cannot be redirected there.
#[cfg(unix)]
#[test]
fn pi_agent_dir_expands_a_quoted_tilde() {
  let home = tempfile::tempdir().unwrap();
  let cwd = tempfile::tempdir().unwrap();
  let output = Command::new(binary())
    .args(["install", "--target", "pi"])
    .current_dir(cwd.path())
    .env("HOME", home.path())
    .env("PI_CODING_AGENT_DIR", "~/agent")
    .env("NO_COLOR", "1")
    .output()
    .expect("run ast-bro");
  assert!(
    output.status.success(),
    "stderr:\n{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(home.path().join("agent/extensions/ast-bro.ts").is_file());
  assert!(!cwd.path().join("~").exists());
}
