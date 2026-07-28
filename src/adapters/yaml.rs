use super::base::{count_parse_errors, LanguageAdapter};
use crate::core::{Declaration, DeclarationKind, ParseResult};
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub struct YamlAdapter;

impl LanguageAdapter for YamlAdapter {
    fn language_name(&self) -> &'static str {
        "yaml"
    }

    fn parse<'a, D: Doc>(&self, path: &Path, source: &[u8], root: Node<'a, D>) -> ParseResult {
        let mut decls = Vec::new();
        _walk_yaml(&root, source, &mut decls);
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

fn _walk_yaml<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<Declaration>) {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "stream" | "document" | "block_node" | "flow_node" => {
            for child in node.children() {
                _walk_yaml(&child, src, out);
            }
        }
        "block_mapping" | "flow_mapping" => {
            for child in node.children() {
                let ck = child.kind();
            let ck: &str = ck.as_ref();
                if ck == "block_mapping_pair" || ck == "flow_pair" {
                    if let Some(decl) = _pair_to_decl(&child, src) {
                        out.push(decl);
                    }
                }
            }
        }
        "block_sequence" | "flow_sequence" => {
            for (i, child) in node.children().enumerate() {
                let ck = child.kind();
            let ck: &str = ck.as_ref();
                if ck == "block_sequence_item" || ck == "flow_node" {
                    let name = format!("[{}]", i);
                    let sig = _node_value_summary(&child, src);
                    let mut item = Declaration {
                        kind: DeclarationKind::Field,
                        name,
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
                    _recurse_yaml(&child, src, &mut item.children);
                    out.push(item);
                }
            }
        }
        _ => {}
    }
}

fn _pair_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let key = node.field("key")?;
    let key_name = String::from_utf8_lossy(&src[key.range()]).trim().to_string();
    let value = node.field("value");
    let sig = value
        .as_ref()
        .map(|v| _node_value_summary(v, src))
        .unwrap_or_else(|| "?".to_string());

    let mut decl = Declaration {
        kind: DeclarationKind::Field,
        name: key_name,
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
        visibility: String::new(),
        start_line: key.start_pos().line() + 1,
        end_line: value.as_ref().map(|v| v.end_pos().line() + 1).unwrap_or(key.end_pos().line() + 1),
        start_byte: key.range().start,
        end_byte: value.as_ref().map(|v| v.range().end).unwrap_or(key.range().end),
        doc_start_byte: key.range().start,
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    };

    if let Some(v) = &value {
        _recurse_yaml(v, src, &mut decl.children);
    }

    Some(decl)
}

fn _recurse_yaml<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<Declaration>) {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "block_mapping" | "flow_mapping" | "block_sequence" | "flow_sequence" | "stream" | "document" | "block_node" | "flow_node" => {
            _walk_yaml(node, src, out);
        }
        _ => {}
    }
}

fn _node_value_summary<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "plain_scalar" | "single_quote_scalar" | "double_quote_scalar" => {
            let text = String::from_utf8_lossy(&src[node.range()]);
            let trimmed = text.trim();
            if trimmed.len() > 40 {
                format!("{}...", &trimmed[..40])
            } else {
                trimmed.to_string()
            }
        }
        "block_scalar" => "block scalar".to_string(),
        "integer_scalar" | "float_scalar" => "number".to_string(),
        "true_scalar" | "false_scalar" => "bool".to_string(),
        "null_scalar" => "null".to_string(),
        "block_mapping" | "flow_mapping" => "mapping".to_string(),
        "block_sequence" | "flow_sequence" => "sequence".to_string(),
        _ => kind.to_string(),
    }
}
