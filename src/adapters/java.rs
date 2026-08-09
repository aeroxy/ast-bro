use super::base::{collapse_ws, count_parse_errors, field_text, LanguageAdapter};
use crate::core::{CallKind, CallSite, Declaration, DeclarationKind, ParseResult};
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub struct JavaAdapter;

impl LanguageAdapter for JavaAdapter {
    fn language_name(&self) -> &'static str {
        "java"
    }

    fn parse<'a, D: Doc>(&self, path: &Path, source: &[u8], root: Node<'a, D>) -> ParseResult {
        let mut decls = Vec::new();
        _walk_top(&root, source, &mut decls);
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

fn _walk_top<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<Declaration>) {
    let mut package_ns: Option<Declaration> = None;
    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        let kind = child.kind();

        if kind == "package_declaration" {
            let ns = _package_to_decl(&child, src);
            out.push(ns);
            package_ns = Some(out.last().unwrap().clone());
        } else if _is_type_node(kind.as_ref()) {
            let type_decl = _type_to_decl(&child, src, None);
            if let Some(ns) = &mut package_ns {
                ns.end_line = type_decl.end_line;
                ns.end_byte = type_decl.end_byte;
                ns.children.push(type_decl);
            } else {
                out.push(type_decl);
            }
        }
    }

    if let Some(ns) = package_ns {
        out[0] = ns;
    }
}

fn _is_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "interface_declaration"
            | "annotation_type_declaration"
            | "enum_declaration"
            | "record_declaration"
    )
}

fn _is_member_node(kind: &str) -> bool {
    matches!(
        kind,
        "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "annotation_type_element_declaration"
            | "field_declaration"
            | "enum_constant"
    )
}

fn _type_node_kind(kind: &str) -> DeclarationKind {
    match kind {
        "class_declaration" => DeclarationKind::Class,
        "interface_declaration" | "annotation_type_declaration" => DeclarationKind::Interface,
        "enum_declaration" => DeclarationKind::Enum,
        "record_declaration" => DeclarationKind::Record,
        _ => DeclarationKind::Class,
    }
}

fn _member_node_kind(kind: &str) -> DeclarationKind {
    match kind {
        "method_declaration" | "annotation_type_element_declaration" => DeclarationKind::Method,
        "constructor_declaration" | "compact_constructor_declaration" => {
            DeclarationKind::Constructor
        }
        "field_declaration" => DeclarationKind::Field,
        "enum_constant" => DeclarationKind::EnumMember,
        _ => DeclarationKind::Field,
    }
}

fn _package_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Declaration {
    let mut name_node = None;
    for c in node.children() {
        if matches!(c.kind().as_ref(), "scoped_identifier" | "identifier") {
            name_node = Some(c);
            break;
        }
    }
    let name = name_node.map(|n| n.text().into_owned()).unwrap_or_default();

    let range = node.range();
    Declaration {
        kind: DeclarationKind::Namespace,
        name: name.clone(),
        signature: format!("package {}", name),
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
        group: None,
        visibility: String::new(),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: range.start,
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    }
}

fn _type_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    parent_kind: Option<&str>,
) -> Declaration {
    let kind = _type_node_kind(node.kind().as_ref());
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let bases = _base_types(node, src);
    let attrs = _annotations(node, src);
    let docs = _javadocs(node);
    let visibility = _visibility(node, src, parent_kind.is_some(), parent_kind);
    let signature = _type_signature(node, src);

    let mut children = Vec::new();

    if node.kind() == "record_declaration" {
        if let Some(params) = node.field("parameters") {
            for p in params.children() {
                if p.kind() == "formal_parameter" {
                    if let Some(field) = _record_component_to_decl(&p, src) {
                        children.push(field);
                    }
                }
            }
        }
    }

    if node.kind() == "enum_declaration" {
        if let Some(body) = node.field("body") {
            for c in body.children() {
                if c.kind() == "enum_constant" {
                    if let Some(m) = _member_to_decl(&c, src, Some("enum_declaration")) {
                        children.push(m);
                    }
                } else if c.kind() == "enum_body_declarations" {
                    for cc in c.children() {
                        if !cc.is_named() {
                            continue;
                        }
                        children.extend(_child_from_body(&cc, src, "enum_declaration"));
                    }
                }
            }
        }
    } else if node.kind() == "annotation_type_declaration" {
        if let Some(body) = node.field("body") {
            for c in body.children() {
                if !c.is_named() {
                    continue;
                }
                children.extend(_child_from_body(&c, src, "annotation_type_declaration"));
            }
        }
    } else if let Some(body) = node.field("body") {
        for c in body.children() {
            if !c.is_named() {
                continue;
            }
            children.extend(_child_from_body(&c, src, node.kind().as_ref()));
        }
    }

    let range = node.range();
    Declaration {
        kind,
        name,
        signature,
        bases,
        attrs,
        docs,
        docs_inside: false,
        group: None,
        visibility,
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: _resolved_doc_start(node),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children,
        calls: Vec::new(),
    }
}

fn _child_from_body<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    parent_kind: &str,
) -> Vec<Declaration> {
    let k = node.kind();
    if _is_type_node(k.as_ref()) {
        return vec![_type_to_decl(node, src, Some(parent_kind))];
    }
    if _is_member_node(k.as_ref()) {
        if let Some(m) = _member_to_decl(node, src, Some(parent_kind)) {
            return vec![m];
        }
    }
    Vec::new()
}

fn _member_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    parent_kind: Option<&str>,
) -> Option<Declaration> {
    let kind = _member_node_kind(node.kind().as_ref());
    let name = _member_name(node, src)?;

    let attrs = _annotations(node, src);
    let docs = _javadocs(node);
    let visibility = _visibility(node, src, true, parent_kind);
    let signature = _member_signature_text(node, src);
    let calls = if matches!(kind, DeclarationKind::Method | DeclarationKind::Constructor) {
        _extract_calls(node, src)
    } else {
        Vec::new()
    };

    let range = node.range();
    Some(Declaration {
        kind,
        name,
        signature,
        bases: Vec::new(),
        attrs,
        docs,
        docs_inside: false,
        group: None,
        visibility,
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: _resolved_doc_start(node),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls,
    })
}

fn _record_component_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name")?;
    let sig = collapse_ws(&node.text());
    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Field,
        name,
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
        group: None,
        visibility: "public".to_string(),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: range.start,
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _type_signature<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let body = node.field("body");
    let end = body.map(|b| b.range().start).unwrap_or(node.range().end);
    let text = String::from_utf8_lossy(&src[node.range().start..end]).to_string();
    let text = _strip_leading_annotations(&text);
    collapse_ws(&text)
        .trim_end_matches(&[' ', '{', ';'][..])
        .to_string()
}

fn _member_signature_text<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let mut cut = node.field("body").map(|b| b.range().start);
    if cut.is_none() {
        for c in node.children() {
            if c.kind() == "block" || c.kind() == "constructor_body" {
                cut = Some(c.range().start);
                break;
            }
        }
    }
    let end = cut.unwrap_or(node.range().end);
    let text = String::from_utf8_lossy(&src[node.range().start..end]).to_string();
    let text = _strip_leading_annotations(&text);
    collapse_ws(&text)
        .trim_end_matches(&[' ', '{', ';'][..])
        .to_string()
}

fn _strip_leading_annotations(text: &str) -> String {
    let mut s = text.trim_start();
    while s.starts_with('@') && !_starts_with_interface_keyword(s) {
        let mut i = 1;
        let bytes = s.as_bytes();
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'_')
        {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b'(' {
            let mut depth = 1;
            i += 1;
            while i < bytes.len() && depth > 0 {
                let ch = bytes[i];
                if ch == b'"' || ch == b'\'' {
                    i = _skip_string_literal(s, i, ch);
                    continue;
                }
                if ch == b'(' {
                    depth += 1;
                } else if ch == b')' {
                    depth -= 1;
                }
                i += 1;
            }
        }
        if i < s.len() {
            s = s[i..].trim_start();
        } else {
            s = "";
            break;
        }
    }
    s.to_string()
}

fn _starts_with_interface_keyword(s: &str) -> bool {
    if !s.starts_with("@interface") {
        return false;
    }
    if s.len() == "@interface".len() {
        return true;
    }
    let nxt = s.as_bytes()["@interface".len()];
    !(nxt.is_ascii_alphanumeric() || nxt == b'_')
}

fn _skip_string_literal(s: &str, mut i: usize, quote: u8) -> usize {
    i += 1;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    i
}

fn _base_types<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();

    if let Some(superclass) = node.field("superclass") {
        for c in superclass.children() {
            if !c.is_named() {
                continue;
            }
            let t = collapse_ws(&c.text()).trim_end_matches(',').to_string();
            if !t.is_empty() {
                out.push(t);
            }
        }
    }
    if let Some(interfaces) = node.field("interfaces") {
        out.extend(_collect_type_list(&interfaces));
    }

    for c in node.children() {
        if c.kind() == "extends_interfaces" {
            out.extend(_collect_type_list(&c));
        }
    }
    out
}

fn _collect_type_list<'a, D: Doc>(container: &Node<'a, D>) -> Vec<String> {
    let mut out = Vec::new();
    for c in container.children() {
        if c.kind() == "type_list" {
            for t_node in c.children() {
                if !t_node.is_named() {
                    continue;
                }
                let t = collapse_ws(&t_node.text())
                    .trim_end_matches(',')
                    .to_string();
                if !t.is_empty() {
                    out.push(t);
                }
            }
        } else if c.is_named() {
            let t = collapse_ws(&c.text()).trim_end_matches(',').to_string();
            if !t.is_empty() {
                out.push(t);
            }
        }
    }
    out
}

fn _modifiers_node<'a, D: Doc>(node: &Node<'a, D>) -> Option<Node<'a, D>> {
    node.children().find(|c| c.kind() == "modifiers")
}

fn _annotations<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(mods) = _modifiers_node(node) {
        for c in mods.children() {
            if c.kind() == "marker_annotation" || c.kind() == "annotation" {
                out.push(collapse_ws(&c.text()));
            }
        }
    }
    out
}

fn _visibility<'a, D: Doc>(
    node: &Node<'a, D>,
    _src: &[u8],
    is_member: bool,
    parent_kind: Option<&str>,
) -> String {
    if let Some(mods) = _modifiers_node(node) {
        for c in mods.children() {
            let k = c.kind();
            if k == "public" || k == "protected" || k == "private" {
                return k.to_string();
            }
        }
    }
    if !is_member {
        return "internal".to_string();
    }
    if let Some(pk) = parent_kind {
        if pk == "interface_declaration" || pk == "annotation_type_declaration" {
            return "public".to_string();
        }
        if pk == "enum_declaration" {
            if node.kind() == "enum_constant" {
                return "public".to_string();
            }
            if node.kind() == "constructor_declaration" {
                return "private".to_string();
            }
        }
    }
    "internal".to_string()
}

fn _javadocs<'a, D: Doc>(node: &Node<'a, D>) -> Vec<String> {
    let mut docs = Vec::new();
    let mut sib = node.prev();
    while let Some(s) = sib {
        if s.kind() == "block_comment" {
            let text = s.text().into_owned();
            if !text.starts_with("/**") {
                break;
            }
            docs.push(text);
            sib = s.prev();
        } else {
            break;
        }
    }
    docs.reverse();
    docs
}

fn _leading_doc_start_byte<'a, D: Doc>(node: &Node<'a, D>) -> Option<usize> {
    let mut first = None;
    let mut sib = node.prev();
    while let Some(s) = sib {
        if s.kind() == "block_comment" && s.text().starts_with("/**") {
            first = Some(s.clone());
            sib = s.prev();
        } else {
            break;
        }
    }
    first.map(|f| f.range().start)
}

fn _resolved_doc_start<'a, D: Doc>(node: &Node<'a, D>) -> usize {
    _leading_doc_start_byte(node).unwrap_or(node.range().start)
}

fn _member_name<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<String> {
    let kind = node.kind();
    if matches!(
        kind.as_ref(),
        "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "annotation_type_element_declaration"
            | "enum_constant"
    ) {
        return field_text(node, "name");
    }
    if kind == "field_declaration" {
        for c in node.children() {
            if c.kind() == "variable_declarator" {
                return field_text(&c, "name");
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Call-site extraction
// ---------------------------------------------------------------------------

fn _extract_calls<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Vec<CallSite> {
    let mut out = Vec::new();
    let body = node.field("body").unwrap_or_else(|| node.clone());
    _walk_calls_in_body(&body, src, &mut out);
    out
}

fn _walk_calls_in_body<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<CallSite>) {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    if matches!(
        kind,
        "class_declaration"
            | "interface_declaration"
            | "annotation_type_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration"
            | "lambda_expression"
    ) {
        return;
    }

    if kind == "method_invocation" {
        if let Some(cs) = _call_site_from_method_invocation(node, src) {
            out.push(cs);
        }
    } else if kind == "object_creation_expression" {
        if let Some(cs) = _call_site_from_object_creation(node, src) {
            out.push(cs);
        }
    }

    for child in node.children() {
        _walk_calls_in_body(&child, src, out);
    }
}

fn _call_site_from_method_invocation<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<CallSite> {
    let name_node = node.field("name")?;
    let name = String::from_utf8_lossy(&src[name_node.range()]).to_string();
    let receiver = node
        .field("object")
        .map(|o| collapse_ws(&String::from_utf8_lossy(&src[o.range()])));
    let line = node.start_pos().line() as u32 + 1;
    Some(CallSite {
        name,
        receiver,
        line,
        kind: CallKind::Call,
    })
}

fn _call_site_from_object_creation<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<CallSite> {
    let type_node = node.field("type")?;
    let raw = String::from_utf8_lossy(&src[type_node.range()]).to_string();
    let name = _last_type_segment(&raw);
    if name.is_empty() {
        return None;
    }
    let line = node.start_pos().line() as u32 + 1;
    Some(CallSite {
        name,
        receiver: None,
        line,
        kind: CallKind::Construct,
    })
}

fn _last_type_segment(text: &str) -> String {
    let head = text.split('<').next().unwrap_or(text).trim();
    head.rsplit('.').next().unwrap_or(head).trim().to_string()
}
