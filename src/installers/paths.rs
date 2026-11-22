use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn home() -> Result<PathBuf, String> {
  dirs::home_dir().ok_or_else(|| "cannot resolve user home directory".to_string())
}

pub fn under_home(relative: impl AsRef<Path>) -> Result<PathBuf, String> {
  Ok(home()?.join(relative))
}

/// Expands a leading `~` the way most tools do: `~` and `~/rest` (also
/// `~\rest` on Windows) are under `home`. Anything else, `~user`
/// included, is returned unchanged.
pub fn expand_tilde(path: &Path, home: &Path) -> PathBuf {
  let Some(text) = path.to_str() else {
    return path.to_path_buf();
  };
  if text == "~" {
    return home.to_path_buf();
  }
  let rest = text
    .strip_prefix("~/")
    .or_else(|| cfg!(windows).then(|| text.strip_prefix("~\\")).flatten());
  match rest {
    Some(rest) => home.join(rest),
    None => path.to_path_buf(),
  }
}

/// True if `name` resolves to an executable on the user's $PATH. Used
/// by adapters' `detect()` so `--all` can skip CLIs the user doesn't
/// actually have installed.
pub fn binary_on_path(name: &str) -> bool {
  Command::new("which")
    .arg(name)
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .map(|s| s.success())
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn under_home_joins_relative() {
    let p = under_home(".claude/CLAUDE.md").unwrap();
    assert!(p.ends_with(".claude/CLAUDE.md"));
    assert!(p.is_absolute());
  }

  #[test]
  fn expand_tilde_expands_home_forms_only() {
    let home = Path::new("/home/me");
    assert_eq!(expand_tilde(Path::new("~"), home), home);
    assert_eq!(expand_tilde(Path::new("~/agent"), home), home.join("agent"));
    assert_eq!(
      expand_tilde(Path::new("/abs/agent"), home),
      Path::new("/abs/agent")
    );
    assert_eq!(
      expand_tilde(Path::new("rel/agent"), home),
      Path::new("rel/agent")
    );
    assert_eq!(
      expand_tilde(Path::new("~user/agent"), home),
      Path::new("~user/agent")
    );
  }
}
