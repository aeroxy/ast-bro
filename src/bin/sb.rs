fn main() -> std::io::Result<()> {
    use std::env::args;
    use std::process::{Command, Stdio};

    let mut child = Command::new("ast-bro")
        .args(args().skip(1))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;

    let status = child.wait()?;
    std::process::exit(status.code().unwrap_or(1))
}
