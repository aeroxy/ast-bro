use super::base::{count_parse_errors, LanguageAdapter};
use crate::core::{Declaration, DeclarationKind, ParseResult};
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub struct JsonAdapter;

impl LanguageAdapter for JsonAdapter {
    fn language_name(&self) -> &'static str {
        "json"
    }

    fn parse<'a, D: Doc>(&self, path: &Path, source: &[u8], root: Node<'a, D>) -> ParseResult {
        let mut decls = Vec::new();
        // Walk root children to find the document → object node
        for child in root.children() {
            if child.is_named() {
                _walk_json(&child, source, &mut decls);
            }
        }
        ParseResult {
            path: path.to_path_buf(),
            language: self.language_name(),
            source: source.to_vec(),
            line_count: source.iter().filter(|&&b| b == b'\n').count() + 1,
            declarations: decls,
            error_count: count_parse_errors(root.clone()),
            imports: Vec::new(),
        }
    }
}

fn _walk_json<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<Declaration>) {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "object" | "document" => {
            for child in node.children() {
                let ck = child.kind();
                let ck: &str = ck.as_ref();
                if ck == "pair" {
                    if let Some(decl) = _pair_to_decl(&child, src) {
                        out.push(decl);
                    }
                }
            }
        }
        "array" => {
            for (i, child) in node.children().enumerate() {
                if child.is_named() {
                    let ck = child.kind();
                    let ck: &str = ck.as_ref();
                    let sig = _value_type_name(ck);
                    let mut idx_decl = Declaration {
                        kind: DeclarationKind::Field,
                        name: format!("[{}]", i),
                        signature: sig,
                        bases: Vec::new(),
                        attrs: Vec::new(),
                        docs: Vec::new(),
                        docs_inside: false,
                        visibility: String::new(),
                        start_line: child.start_pos().line() + 1,
                        end_line: child.end_pos().line() + 1,
                        start_byte: child.range().start,
                        end_byte: child.range().end,
                        doc_start_byte: child.range().start,
                        native_kind: None,
                        modifiers: Vec::new(),
                        deprecated: false,
                        children: Vec::new(),
                        calls: Vec::new(),
                    };
                    if ck == "object" || ck == "array" {
                        _walk_json(&child, src, &mut idx_decl.children);
                    }
                    out.push(idx_decl);
                }
            }
        }
        _ => {}
    }
}

fn _pair_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let key = node.field("key")?;
    let value = node.field("value")?;
    let key_name = String::from_utf8_lossy(&src[key.range()])
        .trim_matches('"')
        .to_string();
    let vk = value.kind();
    let vk: &str = vk.as_ref();
    let val_type = _value_type_name(vk);

    let mut decl = Declaration {
        kind: DeclarationKind::Field,
        name: key_name,
        signature: val_type,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
        visibility: String::new(),
        start_line: key.start_pos().line() + 1,
        end_line: value.end_pos().line() + 1,
        start_byte: key.range().start,
        end_byte: value.range().end,
        doc_start_byte: key.range().start,
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    };

    if vk == "object" || vk == "array" {
        _walk_json(&value, src, &mut decl.children);
    }

    Some(decl)
}

fn _value_type_name(kind: &str) -> String {
    match kind {
        "string" => "string".to_string(),
        "number" => "number".to_string(),
        "true" | "false" => "bool".to_string(),
        "null" => "null".to_string(),
        "object" => "object".to_string(),
        "array" => "array".to_string(),
        other => other.to_string(),
    }
}
