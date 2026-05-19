//! MCP (Model Context Protocol) server over stdio.
//!
//! Implements the small subset of JSON-RPC 2.0 that MCP clients need to
//! discover and invoke ast-bro's existing operations as tools:
//! `initialize`, `tools/list`, `tools/call`, plus `ping` and the
//! `notifications/initialized` notification.
//!
//! Transport: line-delimited JSON on stdin/stdout. One message per line.

mod protocol;
pub(crate) mod tools;

use serde_json::{json, Value};
use std::io::{BufRead, Write};

use protocol::{
    Request, Response, INTERNAL_ERROR, INVALID_PARAMS, METHOD_NOT_FOUND, PARSE_ERROR,
};

/// MCP protocol revision we advertise. Matches the spec at the time of
/// implementation; bump when adopting newer revisions.
const PROTOCOL_VERSION: &str = "2025-06-18";

pub fn run() -> i32 {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("ast-bro mcp: stdin read error: {}", e);
                return 1;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        match serde_json::from_str::<Request>(line) {
            Ok(req) => {
                if let Some(resp) = handle(req) {
                    if let Err(e) = write_message(&mut out, &resp) {
                        eprintln!("ast-bro mcp: stdout write error: {}", e);
                        return 1;
                    }
                }
            }
            Err(e) => {
                let resp = Response::err(
                    Value::Null,
                    PARSE_ERROR,
                    format!("parse error: {}", e),
                );
                if let Err(e) = write_message(&mut out, &resp) {
                    eprintln!("ast-bro mcp: stdout write error: {}", e);
                    return 1;
                }
            }
        }
    }
    0
}

fn write_message<W: Write>(out: &mut W, resp: &Response) -> std::io::Result<()> {
    let line = serde_json::to_string(resp).expect("Response always serialises");
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

fn handle(req: Request) -> Option<Response> {
    let is_notification = req.is_notification();
    let id = req.id.clone().unwrap_or(Value::Null);
    let method = req.method.as_str();

    // Notifications never get a response.
    if is_notification {
        match method {
            "notifications/initialized" | "notifications/cancelled" => {}
            _ => eprintln!("ast-bro mcp: ignoring unknown notification: {}", method),
        }
        return None;
    }

    let result = match method {
        "initialize" => Ok(initialize_result()),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools::list()),
        "tools/call" => tools_call(req.params),
        "resources/list" => Ok(json!({ "resources": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        _ => Err((METHOD_NOT_FOUND, format!("method not found: {}", method))),
    };

    Some(match result {
        Ok(v) => Response::ok(id, v),
        Err((code, msg)) => Response::err(id, code, msg),
    })
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "ast-bro",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": "Structural code outlines via tree-sitter. Use `map` for one file's shape, `digest` for an unfamiliar directory, `show` to extract a single symbol's source, and `implements` to find subclasses of a type."
    })
}

fn tools_call(params: Value) -> Result<Value, (i32, String)> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or((INVALID_PARAMS, "missing `name`".into()))?
        .to_string();
    let args = params.get("arguments").cloned().unwrap_or(Value::Object(Default::default()));

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tools::call(&name, args)
    }));

    match result {
        Ok(tools::CallResult::Text(text)) => Ok(json!({
            "content": [ { "type": "text", "text": text } ],
            "isError": false
        })),
        Ok(tools::CallResult::Error(msg)) => Ok(json!({
            "content": [ { "type": "text", "text": msg } ],
            "isError": true
        })),
        Err(_) => Err((INTERNAL_ERROR, format!("tool `{}` panicked", name))),
    }
}

