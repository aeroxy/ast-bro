use std::path::Path;

use super::event::{Decision, ToolCallEvent};

#[derive(Debug, Clone)]
pub struct DecideOpts {
    pub min_lines: usize,
    pub always: bool,
}

pub fn decide(event: &ToolCallEvent, opts: &DecideOpts) -> Decision {
    if event.tool_name != "Read" {
        return Decision::PassThrough;
    }
    let Some(path) = event.file_path.as_deref() else {
        return Decision::PassThrough;
    };
    if event.has_offset_or_limit {
        return Decision::PassThrough;
    }
    if !crate::main_helpers::can_parse_for_hook(path) {
        return Decision::PassThrough;
    }
    if !opts.always {
        match line_count_at_least(path, opts.min_lines) {
            Ok(true) => {}
            _ => return Decision::PassThrough,
        }
    }
    match render_map_for(path) {
        Some(content) => Decision::Substitute {
            content: format!(
                "{}\n# ast-bro substituted full file. Re-read with offset/limit, or\n# `ast-bro show <file> <symbol>` for a body.\n",
                content
            ),
        },
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

fn render_map_for(path: &Path) -> Option<String> {
    let res = crate::main_helpers::parse_file_for_hook(path)?;
    Some(crate::core::render_map(
        &res,
        &crate::core::MapOptions::default(),
    ))
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

    #[test]
    fn pass_through_for_non_read_tool() {
        let d = decide(&ev("Bash", Some(PathBuf::from("a.rs")), false), &opts());
        assert!(matches!(d, Decision::PassThrough));
    }

    #[test]
    fn pass_through_when_offset_or_limit_set() {
        let d = decide(&ev("Read", Some(PathBuf::from("a.rs")), true), &opts());
        assert!(matches!(d, Decision::PassThrough));
    }

    #[test]
    fn pass_through_for_unsupported_extension() {
        let d = decide(
            &ev("Read", Some(PathBuf::from("img.png")), false),
            &opts(),
        );
        assert!(matches!(d, Decision::PassThrough));
    }

    #[test]
    fn pass_through_when_below_threshold() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("small.rs");
        std::fs::write(&p, "fn main() {}\n").unwrap();
        let d = decide(&ev("Read", Some(p), false), &opts());
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
        let d = decide(&ev("Read", Some(p), false), &opts());
        match d {
            Decision::Substitute { content } => {
                assert!(content.contains("# ast-bro substituted"));
                assert!(content.contains("fn f"));
            }
            other => panic!("unexpected: {:?}", other),
        }
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
        );
        assert!(matches!(d, Decision::Substitute { .. }));
    }
}
