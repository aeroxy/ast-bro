fn main() {
    use std::env::args;
    use std::io::ErrorKind;
    use std::process::{Command, Stdio};

    // Try same directory first (e.g. ./target/release/ast-bro), then fall back to PATH.
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(format!("ast-bro{}", std::env::consts::EXE_SUFFIX))));
    let program = exe_dir
        .filter(|p| p.exists())
        .unwrap_or_else(|| "ast-bro".into());

    let res = Command::new(&program)
        .args(args().skip(1))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn();

    match res {
        Ok(mut child) => match child.wait() {
            Ok(status) => std::process::exit(status.code().unwrap_or(1)),
            Err(e) => {
                eprintln!("error: failed to wait on 'ast-bro': {}", e);
                std::process::exit(1);
            }
        },
        Err(e) if e.kind() == ErrorKind::NotFound => {
            eprintln!("error: 'ast-bro' binary not found.");
            eprintln!();
            eprintln!("'ast-outline' is now a compatibility alias for 'ast-bro'.");
            eprintln!("Please install 'ast-bro' to continue:");
            eprintln!("  - cargo install ast-bro");
            eprintln!("  - npm install -g ast-bro");
            eprintln!("  - pip install ast-bro");
            eprintln!("  - brew install aeroxy/tap/ast-bro");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: failed to execute 'ast-bro': {}", e);
            std::process::exit(1);
        }
    }
}
