fn main() {
    use std::env::args;
    use std::io::ErrorKind;
    use std::process::{Command, Stdio};

    let res = Command::new("ast-bro")
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
            eprintln!("");
            eprintln!("'sb' is now a compatibility alias for 'ast-bro'.");
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
