pub mod event;
pub mod decide;
pub mod claude_code;
pub mod gemini;

use decide::DecideOpts;

pub fn run(protocol: &str, min_lines: usize, always: bool) -> i32 {
    let opts = DecideOpts { min_lines, always };
    match protocol {
        "claude-code" => claude_code::run(opts),
        "gemini" => gemini::run(opts),
        other => {
            eprintln!("ast-bro hook: unknown --protocol '{}'", other);
            2
        }
    }
}
