use super::base::{collapse_ws, count_parse_errors, field_text, LanguageAdapter};
use crate::core::{CallKind, CallSite, Declaration, DeclarationKind, ParseResult};
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub struct KotlinAdapter;

impl LanguageAdapter for KotlinAdapter {
    fn language_name(&self) -> &'static str {
        "kotlin"
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

        if kind == "package_header" {
            let ns = _package_to_decl(&child, src);
            out.push(ns);
            package_ns = Some(out.last().unwrap().clone());
        } else if _is_top_decl(kind.as_ref()) {
            if let Some(decl) = _decl_from_node(&child, src, None) {
                if let Some(ns) = &mut package_ns {
                    ns.end_line = decl.end_line;
                    ns.end_byte = decl.end_byte;
                    ns.children.push(decl);
                } else {
                    out.push(decl);
                }
            }
        }
    }

    if let Some(ns) = package_ns {
        out[0] = ns;
    }
}

fn _is_top_decl(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "object_declaration"
            | "function_declaration"
            | "property_declaration"
            | "type_alias"
    )
}

fn _package_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Declaration {
    let mut name_node = None;
    for c in node.children() {
        if c.kind() == "qualified_identifier" || c.kind() == "identifier" {
            name_node = Some(c);
            break;
        }
    }
    let name = name_node
        .map(|n| collapse_ws(&n.text()))
        .unwrap_or_default();

    let signature = if name.is_empty() {
        "package".to_string()
    } else {
        format!("package {}", name)
    };
    let range = node.range();
    Declaration {
        kind: DeclarationKind::Namespace,
        name: name.clone(),
        signature,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
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

fn _decl_from_node<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    parent_kind: Option<&str>,
) -> Option<Declaration> {
    let k = node.kind();
    if k == "class_declaration" {
        return Some(_type_to_decl(node, src, parent_kind));
    }
    if k == "object_declaration" {
        return Some(_object_to_decl(node, src, parent_kind));
    }
    if k == "companion_object" {
        return Some(_companion_to_decl(node, src, parent_kind));
    }
    if k == "function_declaration" {
        return Some(_function_to_decl(node, src, parent_kind));
    }
    if k == "property_declaration" {
        return _property_to_decl(node, src, parent_kind);
    }
    if k == "secondary_constructor" {
        return Some(_secondary_ctor_to_decl(node, src, parent_kind));
    }
    if k == "type_alias" {
        return _type_alias_to_decl(node, src);
    }
    if k == "enum_entry" {
        return _enum_entry_to_decl(node, src);
    }
    None
}

fn _type_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    _parent_kind: Option<&str>,
) -> Declaration {
    let kind = _class_decl_kind(node);
    let name = _decl_name(node);
    let bases = _delegation_bases(node, src);
    let attrs = _annotations(node, src);
    let docs = _kdocs(node);
    let visibility = _visibility(node);
    let signature = _type_signature(node, src);

    let mut children = _primary_ctor_fields(node, src);
    children.extend(_collect_type_children(node, src, kind.as_str()));

    let range = node.range();
    Declaration {
        kind,
        name,
        signature,
        bases,
        attrs,
        docs,
        docs_inside: false,
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

fn _object_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    _parent_kind: Option<&str>,
) -> Declaration {
    let name = _decl_name(node);
    let bases = _delegation_bases(node, src);
    let attrs = _annotations(node, src);
    let docs = _kdocs(node);
    let visibility = _visibility(node);
    let signature = _type_signature(node, src);

    let children = _collect_type_children(node, src, DeclarationKind::Class.as_str());

    let range = node.range();
    Declaration {
        kind: DeclarationKind::Class,
        name,
        signature,
        bases,
        attrs,
        docs,
        docs_inside: false,
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

fn _companion_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    _parent_kind: Option<&str>,
) -> Declaration {
    let name = _decl_name_or(node, "Companion");
    let bases = _delegation_bases(node, src);
    let attrs = _annotations(node, src);
    let docs = _kdocs(node);
    let visibility = _visibility(node);
    let signature = _type_signature(node, src);

    let children = _collect_type_children(node, src, DeclarationKind::Class.as_str());

    let range = node.range();
    Declaration {
        kind: DeclarationKind::Class,
        name,
        signature,
        bases,
        attrs,
        docs,
        docs_inside: false,
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

fn _class_decl_kind<'a, D: Doc>(node: &Node<'a, D>) -> DeclarationKind {
    for c in node.children() {
        if c.kind() == "interface" {
            return DeclarationKind::Interface;
        }
        if c.kind() == "class" {
            break;
        }
    }
    if let Some(mods) = _modifiers_node(node) {
        for m in mods.children() {
            if m.kind() == "class_modifier" {
                let token = m.text().trim().to_string();
                if token == "enum" {
                    return DeclarationKind::Enum;
                }
                if token == "data" {
                    return DeclarationKind::Record;
                }
                if token == "annotation" {
                    return DeclarationKind::Interface;
                }
            }
        }
    }
    DeclarationKind::Class
}

fn _type_body<'a, D: Doc>(node: &Node<'a, D>) -> Option<Node<'a, D>> {
    node.children()
        .find(|c| c.kind() == "class_body" || c.kind() == "enum_class_body")
}

fn _collect_type_children<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    kind: &str,
) -> Vec<Declaration> {
    let mut out = Vec::new();
    if let Some(body) = _type_body(node) {
        for c in body.children() {
            if !c.is_named() {
                continue;
            }
            if let Some(decl) = _decl_from_node(&c, src, Some(kind)) {
                out.push(decl);
            }
        }
    }
    out
}

fn _primary_ctor_fields<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Vec<Declaration> {
    let mut out = Vec::new();
    let pc = node.children().find(|c| c.kind() == "primary_constructor");
    if let Some(pc_node) = pc {
        let params = pc_node.children().find(|c| c.kind() == "class_parameters");
        if let Some(p_node) = params {
            for cp in p_node.children() {
                if cp.kind() == "class_parameter" {
                    if let Some(decl) = _class_parameter_to_field(&cp, src) {
                        out.push(decl);
                    }
                }
            }
        }
    }
    out
}

fn _class_parameter_to_field<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let mut is_prop = false;
    for c in node.children() {
        if c.kind() == "val" || c.kind() == "var" {
            is_prop = true;
            break;
        }
    }
    if !is_prop {
        return None;
    }

    let name_node = node.children().find(|c| c.kind() == "identifier")?;
    let name = name_node.text().into_owned();
    let attrs = _annotations(node, src);
    let sig = collapse_ws(&_strip_leading_annotations(&node.text()));

    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Field,
        name,
        signature: sig,
        bases: Vec::new(),
        attrs,
        docs: Vec::new(),
        docs_inside: false,
        visibility: _visibility(node),
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

fn _function_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    parent_kind: Option<&str>,
) -> Declaration {
    let kind = if parent_kind.is_some() {
        DeclarationKind::Method
    } else {
        DeclarationKind::Function
    };
    let name = _decl_name(node);
    let attrs = _annotations(node, src);
    let docs = _kdocs(node);
    let visibility = _visibility(node);
    let signature = _callable_signature(node, src);
    let calls = _extract_calls(node, src);

    let range = node.range();
    Declaration {
        kind,
        name,
        signature,
        bases: Vec::new(),
        attrs,
        docs,
        docs_inside: false,
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
    }
}

fn _secondary_ctor_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    _parent_kind: Option<&str>,
) -> Declaration {
    let attrs = _annotations(node, src);
    let docs = _kdocs(node);
    let visibility = _visibility(node);
    let signature = _callable_signature(node, src);
    let calls = _extract_calls(node, src);

    let range = node.range();
    Declaration {
        kind: DeclarationKind::Constructor,
        name: "constructor".to_string(),
        signature,
        bases: Vec::new(),
        attrs,
        docs,
        docs_inside: false,
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
    }
}

fn _property_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    _parent_kind: Option<&str>,
) -> Option<Declaration> {
    let name = _property_name(node)?;
    let mut has_acc = false;
    for c in node.children() {
        if c.kind() == "getter" || c.kind() == "setter" {
            has_acc = true;
            break;
        }
    }
    let kind = if has_acc {
        DeclarationKind::Property
    } else {
        DeclarationKind::Field
    };
    let attrs = _annotations(node, src);
    let docs = _kdocs(node);
    let visibility = _visibility(node);
    let signature = _property_signature(node, src);

    let range = node.range();
    Some(Declaration {
        kind,
        name,
        signature,
        bases: Vec::new(),
        attrs,
        docs,
        docs_inside: false,
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
        calls: Vec::new(),
    })
}

fn _property_name<'a, D: Doc>(node: &Node<'a, D>) -> Option<String> {
    for c in node.children() {
        if c.kind() == "variable_declaration" || c.kind() == "multi_variable_declaration" {
            for cc in c.children() {
                if cc.kind() == "identifier" {
                    return Some(cc.text().into_owned());
                }
            }
        }
    }
    None
}

fn _enum_entry_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name_node = node.children().find(|c| c.kind() == "identifier")?;
    let name = name_node.text().into_owned();
    let attrs = _annotations(node, src);
    let docs = _kdocs(node);
    let sig = collapse_ws(&_strip_leading_annotations(&node.text()))
        .trim_end_matches(',')
        .to_string();

    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::EnumMember,
        name,
        signature: sig,
        bases: Vec::new(),
        attrs,
        docs,
        docs_inside: false,
        visibility: "public".to_string(),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: _resolved_doc_start(node),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _type_alias_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name_node = node.children().find(|c| c.kind() == "identifier")?;
    let name = name_node.text().into_owned();
    let attrs = _annotations(node, src);
    let docs = _kdocs(node);
    let visibility = _visibility(node);
    let sig = collapse_ws(&_strip_leading_annotations(&node.text()))
        .trim_end_matches(';')
        .to_string();

    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Delegate,
        name,
        signature: sig,
        bases: Vec::new(),
        attrs,
        docs,
        docs_inside: false,
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
        calls: Vec::new(),
    })
}

fn _type_signature<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let body = _type_body(node);
    let end = body.map(|b| b.range().start).unwrap_or(node.range().end);
    let text = String::from_utf8_lossy(&src[node.range().start..end]).to_string();
    let text = _strip_leading_annotations(&text);
    collapse_ws(&text)
        .trim_end_matches(&[' ', '{', ';'][..])
        .to_string()
}

fn _callable_signature<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let mut cut = None;
    for c in node.children() {
        if c.kind() == "function_body" || c.kind() == "block" {
            cut = Some(c.range().start);
            break;
        }
    }
    let end = cut.unwrap_or(node.range().end);
    let text = String::from_utf8_lossy(&src[node.range().start..end]).to_string();
    let text = _strip_leading_annotations(&text);
    collapse_ws(&text)
        .trim_end_matches(&[' ', '{', ';', '='][..])
        .to_string()
}

fn _property_signature<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let mut cut = None;
    for c in node.children() {
        if c.kind() == "getter" || c.kind() == "setter" {
            cut = Some(c.range().start);
            break;
        }
    }
    let end = cut.unwrap_or(node.range().end);
    let text = String::from_utf8_lossy(&src[node.range().start..end]).to_string();
    let text = _strip_leading_annotations(&text);
    collapse_ws(&text)
        .trim_end_matches(&[' ', '{', ';'][..])
        .to_string()
}

fn _delegation_bases<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    for c in node.children() {
        if c.kind() == "delegation_specifiers" {
            return _collect_delegation_types(&c);
        }
    }
    Vec::new()
}

fn _collect_delegation_types<'a, D: Doc>(container: &Node<'a, D>) -> Vec<String> {
    let mut out = Vec::new();
    for spec in container.children() {
        if spec.kind() == "delegation_specifier" {
            if let Some(t) = _delegation_type_text(&spec) {
                out.push(t);
            }
        }
    }
    out
}

fn _delegation_type_text<'a, D: Doc>(spec: &Node<'a, D>) -> Option<String> {
    for c in spec.children() {
        if !c.is_named() {
            continue;
        }
        if c.kind() == "constructor_invocation" {
            for cc in c.children() {
                if cc.kind() == "user_type" {
                    return Some(collapse_ws(&cc.text()));
                }
            }
        } else if c.kind() == "user_type" {
            return Some(collapse_ws(&c.text()));
        } else if c.kind() == "explicit_delegation" {
            for cc in c.children() {
                if cc.kind() == "user_type" {
                    return Some(collapse_ws(&cc.text()));
                }
            }
        }
    }
    None
}

fn _modifiers_node<'a, D: Doc>(node: &Node<'a, D>) -> Option<Node<'a, D>> {
    node.children().find(|c| c.kind() == "modifiers")
}

fn _annotations<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(mods) = _modifiers_node(node) {
        for c in mods.children() {
            if c.kind() == "annotation" {
                out.push(collapse_ws(&c.text()));
            }
        }
    }
    out
}

fn _visibility<'a, D: Doc>(node: &Node<'a, D>) -> String {
    if let Some(mods) = _modifiers_node(node) {
        for c in mods.children() {
            if c.kind() == "visibility_modifier" {
                for cc in c.children() {
                    let k = cc.kind();
                    if k == "public" || k == "protected" || k == "private" || k == "internal" {
                        return k.to_string();
                    }
                }
            }
        }
    }
    "public".to_string()
}

fn _kdocs<'a, D: Doc>(node: &Node<'a, D>) -> Vec<String> {
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

fn _strip_leading_annotations(text: &str) -> String {
    let mut s = text.trim_start();
    while s.starts_with('@') {
        let mut i = 1;
        let bytes = s.as_bytes();
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'_')
        {
            i += 1;
        }
        if i < bytes.len() && bytes[i] == b':' {
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.' || bytes[i] == b'_')
            {
                i += 1;
            }
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

// tree-sitter-kotlin (fwcd) does NOT expose a "name" field on
// function_declaration / class_declaration / object_declaration. The name is
// a positional `simple_identifier` (or `type_identifier`) child sitting after
// the keyword token. Try the field first for forward-compat with grammars
// that do define it, then fall back to the first identifier child.
fn _decl_name<'a, D: Doc>(node: &Node<'a, D>) -> String {
    _decl_name_or(node, "?")
}

fn _decl_name_or<'a, D: Doc>(node: &Node<'a, D>, fallback: &str) -> String {
    if let Some(n) = field_text(node, "name") {
        return n;
    }
    for c in node.children() {
        let k = c.kind();
        let k: &str = k.as_ref();
        if matches!(k, "simple_identifier" | "type_identifier" | "identifier") {
            return c.text().into_owned();
        }
    }
    fallback.to_string()
}

// ---------------------------------------------------------------------------
// Call-site extraction
// ---------------------------------------------------------------------------

fn _extract_calls<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Vec<CallSite> {
    let mut out = Vec::new();
    let body = _callable_body(node).unwrap_or_else(|| node.clone());
    _walk_calls_in_body(&body, src, &mut out);
    out
}

fn _callable_body<'a, D: Doc>(node: &Node<'a, D>) -> Option<Node<'a, D>> {
    node.children()
        .find(|c| c.kind() == "function_body" || c.kind() == "block")
}

fn _walk_calls_in_body<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<CallSite>) {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    if matches!(
        kind,
        "function_declaration"
            | "anonymous_function"
            | "lambda_literal"
            | "class_declaration"
            | "object_declaration"
            | "secondary_constructor"
            | "getter"
            | "setter"
    ) {
        return;
    }

    if kind == "call_expression" {
        if let Some(cs) = _call_site_from_call_kt(node, src) {
            out.push(cs);
        }
    }

    for child in node.children() {
        _walk_calls_in_body(&child, src, out);
    }
}

fn _call_site_from_call_kt<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<CallSite> {
    // tree-sitter-kotlin's call_expression is positional: first named child is
    // the callee expression, second is the call_suffix carrying arguments.
    let callee = node.children().find(|c| c.is_named() && c.kind() != "call_suffix")?;
    let (name, receiver) = _extract_callee_name_kt(&callee, src)?;
    let line = node.start_pos().line() as u32 + 1;
    Some(CallSite {
        name,
        receiver,
        line,
        kind: CallKind::Call,
    })
}

fn _extract_callee_name_kt<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<(String, Option<String>)> {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "simple_identifier" | "identifier" => {
            Some((String::from_utf8_lossy(&src[node.range()]).to_string(), None))
        }
        "navigation_expression" => {
            // A navigation_expression has the receiver as the first named
            // child and a navigation_suffix containing the member name.
            let mut iter = node.children().filter(|c| c.is_named());
            let receiver_node = iter.next()?;
            let mut name = None;
            for c in iter {
                if c.kind() == "navigation_suffix" {
                    for cc in c.children() {
                        let k = cc.kind();
                        if cc.is_named() && (k == "simple_identifier" || k == "identifier") {
                            name = Some(String::from_utf8_lossy(&src[cc.range()]).to_string());
                            break;
                        }
                    }
                }
            }
            let name = name?;
            let recv = collapse_ws(&String::from_utf8_lossy(&src[receiver_node.range()]));
            Some((name, Some(recv)))
        }
        _ => {
            let raw = String::from_utf8_lossy(&src[node.range()]).to_string();
            let trimmed = raw.split('<').next().unwrap_or(&raw).trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some((trimmed, None))
            }
        }
    }
}
