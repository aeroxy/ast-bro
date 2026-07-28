use crate::core::{Declaration, DeclarationKind, ParseResult};
use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

static FN_RE: OnceLock<Regex> = OnceLock::new();
static CLASS_RE: OnceLock<Regex> = OnceLock::new();
static ENUM_RE: OnceLock<Regex> = OnceLock::new();

pub fn parse_powershell(path: &Path, source: &[u8]) -> ParseResult {
    let text = String::from_utf8_lossy(source);
    let line_count = source.iter().filter(|&&b| b == b'\n').count() + 1;

    let mut decls = Vec::new();

    let fn_re = FN_RE.get_or_init(|| {
        Regex::new(r"(?im)^\s*(function|filter|workflow)\s+(\w[\w-]*)\s*").unwrap()
    });

    let class_re = CLASS_RE.get_or_init(|| {
        Regex::new(r"(?im)^\s*class\s+(\w[\w-]*)\s*").unwrap()
    });

    let enum_re = ENUM_RE.get_or_init(|| {
        Regex::new(r"(?im)^\s*enum\s+(\w[\w-]*)\s*").unwrap()
    });

    for caps in fn_re.captures_iter(&text) {
        let kind_str = caps.get(1).unwrap().as_str();
        let name = caps.get(2).unwrap().as_str().to_string();
        let start_pos = caps.get(0).unwrap().start();
        let start_line = text[..start_pos].chars().filter(|&c| c == '\n').count() + 1;

        let kind = match kind_str {
            "filter" => DeclarationKind::Function,
            "workflow" => DeclarationKind::Method,
            _ => DeclarationKind::Function,
        };

        decls.push(Declaration {
            kind,
            name,
            signature: format!("{} {}", kind_str, caps.get(2).unwrap().as_str()),
            bases: Vec::new(),
            attrs: Vec::new(),
            docs: Vec::new(),
            docs_inside: false,
            visibility: String::new(),
            start_line,
            end_line: start_line,
            start_byte: start_pos,
            end_byte: caps.get(0).unwrap().end(),
            doc_start_byte: start_pos,
            native_kind: Some(kind_str.to_string()),
            modifiers: Vec::new(),
            deprecated: false,
            children: Vec::new(),
            calls: Vec::new(),
        });
    }

    for caps in class_re.captures_iter(&text) {
        let name = caps.get(1).unwrap().as_str().to_string();
        let start_pos = caps.get(0).unwrap().start();
        let start_line = text[..start_pos].chars().filter(|&c| c == '\n').count() + 1;

        decls.push(Declaration {
            kind: DeclarationKind::Class,
            name: name.clone(),
            signature: format!("class {}", name),
            bases: Vec::new(),
            attrs: Vec::new(),
            docs: Vec::new(),
            docs_inside: false,
            visibility: String::new(),
            start_line,
            end_line: start_line,
            start_byte: start_pos,
            end_byte: caps.get(0).unwrap().end(),
            doc_start_byte: start_pos,
            native_kind: Some("class_definition".to_string()),
            modifiers: Vec::new(),
            deprecated: false,
            children: Vec::new(),
            calls: Vec::new(),
        });
    }

    for caps in enum_re.captures_iter(&text) {
        let name = caps.get(1).unwrap().as_str().to_string();
        let start_pos = caps.get(0).unwrap().start();
        let start_line = text[..start_pos].chars().filter(|&c| c == '\n').count() + 1;

        decls.push(Declaration {
            kind: DeclarationKind::Enum,
            name: name.clone(),
            signature: format!("enum {}", name),
            bases: Vec::new(),
            attrs: Vec::new(),
            docs: Vec::new(),
            docs_inside: false,
            visibility: String::new(),
            start_line,
            end_line: start_line,
            start_byte: start_pos,
            end_byte: caps.get(0).unwrap().end(),
            doc_start_byte: start_pos,
            native_kind: Some("enum_definition".to_string()),
            modifiers: Vec::new(),
            deprecated: false,
            children: Vec::new(),
            calls: Vec::new(),
        });
    }

    ParseResult {
        path: path.to_path_buf(),
        language: "powershell",
        source: source.to_vec(),
        line_count,
        declarations: decls,
        error_count: 0,
        imports: Vec::new(),
    }
}
