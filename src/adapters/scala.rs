use super::base::{collapse_ws, count_parse_errors, field_text, LanguageAdapter};
use crate::core::{CallKind, CallSite, Declaration, DeclarationKind, ParseResult};
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub struct ScalaAdapter;

impl LanguageAdapter for ScalaAdapter {
    fn language_name(&self) -> &'static str {
        "scala"
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
    let mut children: Vec<Node<'a, D>> = Vec::new();
    for c in node.children() {
        if c.is_named() {
            children.push(c);
        }
    }

    let mut braceless_pkgs = Vec::new();
    let mut i = 0;
    while i < children.len() {
        let c = &children[i];
        if c.kind() == "package_clause" && c.field("body").is_none() {
            braceless_pkgs.push(c.clone());
            i += 1;
        } else {
            break;
        }
    }

    let mut package_ns = None;
    if !braceless_pkgs.is_empty() {
        let ns = _dotted_package_namespace(&braceless_pkgs, src);
        out.push(ns);
        package_ns = Some(out[0].clone());
    }

    let mut end_line = 0;
    let mut end_byte = 0;

    while i < children.len() {
        let c = &children[i];
        let k = c.kind();
        if k == "package_clause" && c.field("body").is_some() {
            let decl = _braced_package_to_decl(c, src);
            end_line = std::cmp::max(end_line, decl.end_line);
            end_byte = std::cmp::max(end_byte, decl.end_byte);
            if let Some(ref mut ns) = package_ns {
                ns.children.push(decl);
            } else {
                out.push(decl);
            }
        } else if _is_decl_node(k.as_ref()) {
            let decls = _decl_from_node(c, src, None);
            for d in decls {
                end_line = std::cmp::max(end_line, d.end_line);
                end_byte = std::cmp::max(end_byte, d.end_byte);
                if let Some(ref mut ns) = package_ns {
                    ns.children.push(d);
                } else {
                    out.push(d);
                }
            }
        }
        i += 1;
    }

    if let Some(mut ns) = package_ns {
        if end_line > 0 {
            ns.end_line = std::cmp::max(ns.end_line, end_line);
            ns.end_byte = std::cmp::max(ns.end_byte, end_byte);
        }
        out[0] = ns;
    }
}

fn _is_decl_node(kind: &str) -> bool {
    matches!(
        kind,
        "class_definition"
            | "trait_definition"
            | "object_definition"
            | "enum_definition"
            | "given_definition"
            | "function_definition"
            | "function_declaration"
            | "val_definition"
            | "val_declaration"
            | "var_definition"
            | "var_declaration"
            | "type_definition"
            | "extension_definition"
            | "package_object"
    )
}

fn _dotted_package_namespace<'a, D: Doc>(pkg_clauses: &[Node<'a, D>], _src: &[u8]) -> Declaration {
    let mut parts = Vec::new();
    for c in pkg_clauses {
        let mut pid = c.field("name");
        if pid.is_none() {
            for child in c.children() {
                if child.kind() == "package_identifier" || child.kind() == "identifier" {
                    pid = Some(child);
                    break;
                }
            }
        }
        if let Some(p) = pid {
            let t = collapse_ws(&p.text());
            if !t.is_empty() {
                parts.push(t);
            }
        }
    }
    let name = parts.join(".");
    let first = &pkg_clauses[0];
    let last = &pkg_clauses[pkg_clauses.len() - 1];

    let signature = if name.is_empty() {
        "package".to_string()
    } else {
        format!("package {}", name)
    };
    Declaration {
        kind: DeclarationKind::Namespace,
        name,
        signature,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
        group: None,
        visibility: String::new(),
        start_line: first.start_pos().line() + 1,
        end_line: last.end_pos().line() + 1,
        start_byte: first.range().start,
        end_byte: last.range().end,
        doc_start_byte: first.range().start,
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    }
}

fn _braced_package_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let pid = node.field("name");
    let name = pid.map(|p| collapse_ws(&p.text())).unwrap_or_default();

    let mut children = Vec::new();
    if let Some(body) = node.field("body") {
        for c in body.children() {
            if !c.is_named() {
                continue;
            }
            let k = c.kind();
            if _is_decl_node(k.as_ref()) {
                children.extend(_decl_from_node(&c, src, None));
            } else if k == "package_clause" && c.field("body").is_some() {
                children.push(_braced_package_to_decl(&c, src));
            }
        }
    }

    let signature = if name.is_empty() {
        "package".to_string()
    } else {
        format!("package {}", name)
    };
    let range = node.range();
    Declaration {
        kind: DeclarationKind::Namespace,
        name,
        signature,
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
        children,
        calls: Vec::new(),
    }
}

fn _decl_from_node<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    parent_kind: Option<&str>,
) -> Vec<Declaration> {
    let t = node.kind();
    if t == "class_definition" || t == "trait_definition" || t == "object_definition" {
        return vec![_type_to_decl(node, src, parent_kind)];
    }
    if t == "enum_definition" {
        return vec![_enum_to_decl(node, src, parent_kind)];
    }
    if t == "given_definition" {
        if let Some(d) = _given_to_decl(node, src, parent_kind) {
            return vec![d];
        }
        return Vec::new();
    }
    if t == "function_definition" || t == "function_declaration" {
        return vec![_function_to_decl(node, src, parent_kind)];
    }
    if matches!(
        t.as_ref(),
        "val_definition" | "var_definition" | "val_declaration" | "var_declaration"
    ) {
        if let Some(d) = _property_to_decl(node, src, parent_kind) {
            return vec![d];
        }
        return Vec::new();
    }
    if t == "type_definition" {
        if let Some(d) = _type_alias_to_decl(node, src) {
            return vec![d];
        }
        return Vec::new();
    }
    if t == "extension_definition" {
        return _extension_to_decls(node, src, parent_kind);
    }
    if t == "package_object" {
        return vec![_package_object_to_decl(node, src)];
    }
    Vec::new()
}

fn _type_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    _parent_kind: Option<&str>,
) -> Declaration {
    let kind = _type_decl_kind(node);
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let bases = _extends_bases(node, src);
    let attrs = _annotations(node, src);
    let docs = _scaladocs(node);
    let visibility = _visibility(node);
    let signature = _type_signature(node, src);

    let mut children = _primary_ctor_fields(node, src, _has_case_keyword(node));
    if let Some(body) = node.field("body") {
        for c in body.children() {
            if !c.is_named() {
                continue;
            }
            if _is_decl_node(c.kind().as_ref()) {
                children.extend(_decl_from_node(&c, src, Some(kind.as_str())));
            }
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

fn _type_decl_kind<'a, D: Doc>(node: &Node<'a, D>) -> DeclarationKind {
    let t = node.kind();
    if t == "trait_definition" {
        return DeclarationKind::Interface;
    }
    if t == "class_definition" && _has_case_keyword(node) {
        return DeclarationKind::Record;
    }
    DeclarationKind::Class
}

fn _has_case_keyword<'a, D: Doc>(node: &Node<'a, D>) -> bool {
    for c in node.children() {
        if c.kind() == "case" {
            return true;
        }
        if c.kind() == "class" || c.kind() == "trait" || c.kind() == "object" {
            break;
        }
    }
    false
}

fn _enum_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    _parent_kind: Option<&str>,
) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let bases = _extends_bases(node, src);
    let attrs = _annotations(node, src);
    let docs = _scaladocs(node);
    let visibility = _visibility(node);
    let signature = _type_signature(node, src);

    let mut children = _primary_ctor_fields(node, src, false);
    if let Some(body) = node.field("body") {
        for c in body.children() {
            if !c.is_named() {
                continue;
            }
            if c.kind() == "enum_case_definitions" {
                children.extend(_enum_case_entries(&c, src));
            } else if _is_decl_node(c.kind().as_ref()) {
                children.extend(_decl_from_node(&c, src, Some("enum_definition")));
            }
        }
    }

    let range = node.range();
    Declaration {
        kind: DeclarationKind::Enum,
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

fn _enum_case_entries<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<Declaration> {
    let mut out = Vec::new();
    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        if c.kind() == "simple_enum_case" || c.kind() == "full_enum_case" {
            let mut name_node = c.field("name");
            if name_node.is_none() {
                for cc in c.children() {
                    if cc.kind() == "identifier" || cc.kind() == "type_identifier" {
                        name_node = Some(cc);
                        break;
                    }
                }
            }
            if let Some(n) = name_node {
                let sig = collapse_ws(&_strip_leading_annotations(&c.text()))
                    .trim_end_matches(',')
                    .to_string();
                let range = c.range();
                out.push(Declaration {
                    kind: DeclarationKind::EnumMember,
                    name: n.text().into_owned(),
                    signature: sig,
                    bases: Vec::new(),
                    attrs: Vec::new(),
                    docs: Vec::new(),
                    docs_inside: false,
                    group: None,
                    visibility: "public".to_string(),
                    start_line: c.start_pos().line() + 1,
                    end_line: c.end_pos().line() + 1,
                    start_byte: range.start,
                    end_byte: range.end,
                    doc_start_byte: range.start,
                    native_kind: None,
                    modifiers: Vec::new(),
                    deprecated: false,
                    children: Vec::new(),
                    calls: Vec::new(),
                });
            }
        }
    }
    out
}

fn _given_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    _parent_kind: Option<&str>,
) -> Option<Declaration> {
    let mut name = field_text(node, "name");
    if name.is_none() || name.as_ref().unwrap().is_empty() {
        for c in node.children() {
            if c.kind() == "type_identifier" || c.kind() == "generic_type" {
                name = Some(format!("given {}", collapse_ws(&c.text())));
                break;
            }
        }
    }
    let name = name.unwrap_or_else(|| "given".to_string());

    let attrs = _annotations(node, src);
    let docs = _scaladocs(node);
    let visibility = _visibility(node);
    let signature = _type_signature(node, src);

    let mut bases = Vec::new();
    for c in node.children() {
        if c.kind() == "type_identifier" || c.kind() == "generic_type" {
            bases.push(collapse_ws(&c.text()));
            break;
        }
    }

    let mut children = Vec::new();
    if let Some(body) = node.field("body") {
        for c in body.children() {
            if !c.is_named() {
                continue;
            }
            if _is_decl_node(c.kind().as_ref()) {
                children.extend(_decl_from_node(&c, src, Some("class_definition")));
            }
        }
    }

    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Class,
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
    })
}

fn _package_object_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let attrs = _annotations(node, src);
    let docs = _scaladocs(node);
    let signature = _type_signature(node, src);

    let mut children = Vec::new();
    if let Some(body) = node.field("body") {
        for c in body.children() {
            if !c.is_named() {
                continue;
            }
            if _is_decl_node(c.kind().as_ref()) {
                children.extend(_decl_from_node(&c, src, Some("class_definition")));
            }
        }
    }

    let range = node.range();
    Declaration {
        kind: DeclarationKind::Class,
        name,
        signature,
        bases: Vec::new(),
        attrs,
        docs,
        docs_inside: false,
        group: None,
        visibility: "public".to_string(),
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

fn _primary_ctor_fields<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    is_case: bool,
) -> Vec<Declaration> {
    let mut out = Vec::new();
    let pc = node.children().find(|c| c.kind() == "primary_constructor");
    if let Some(p) = pc {
        let params = p.children().find(|c| c.kind() == "class_parameters");
        if let Some(ps) = params {
            for cp in ps.children() {
                if cp.kind() == "class_parameter" {
                    if let Some(d) = _class_parameter_to_field(&cp, src, is_case) {
                        out.push(d);
                    }
                }
            }
        }
    }
    out
}

fn _class_parameter_to_field<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    is_case: bool,
) -> Option<Declaration> {
    let mut has_val_var = false;
    for c in node.children() {
        if c.kind() == "val" || c.kind() == "var" {
            has_val_var = true;
            break;
        }
    }
    if !has_val_var && !is_case {
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
        group: None,
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
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let attrs = _annotations(node, src);
    let docs = _scaladocs(node);
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
    }
}

fn _property_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    _parent_kind: Option<&str>,
) -> Option<Declaration> {
    let name = _property_name(node, src)?;
    let attrs = _annotations(node, src);
    let docs = _scaladocs(node);
    let visibility = _visibility(node);
    let signature = _property_signature(node, src);

    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Field,
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
        calls: Vec::new(),
    })
}

fn _property_name<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<String> {
    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        if c.kind() == "identifier" {
            return Some(c.text().into_owned());
        }
        if c.kind() == "tuple_pattern" {
            for cc in c.children() {
                if cc.kind() == "identifier" {
                    return Some(cc.text().into_owned());
                }
            }
        }
    }
    None
}

fn _type_alias_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name_node = node.children().find(|c| c.kind() == "type_identifier")?;
    let name = name_node.text().into_owned();
    let attrs = _annotations(node, src);
    let docs = _scaladocs(node);
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
        calls: Vec::new(),
    })
}

fn _extension_to_decls<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    parent_kind: Option<&str>,
) -> Vec<Declaration> {
    let mut receiver_text = String::new();
    for c in node.children() {
        if c.kind() == "parameters" {
            receiver_text = collapse_ws(&c.text());
            break;
        }
    }
    let prefix = if receiver_text.is_empty() {
        "extension ".to_string()
    } else {
        format!("extension {} ", receiver_text)
    };

    let mut out = Vec::new();
    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        if c.kind() == "function_definition" || c.kind() == "function_declaration" {
            let mut fn_decl = _function_to_decl(&c, src, parent_kind);
            fn_decl.signature = format!("{}{}", prefix, fn_decl.signature);
            out.push(fn_decl);
        }
    }
    out
}

fn _type_signature<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let body = node.field("body");
    let end = body.map(|b| b.range().start).unwrap_or(node.range().end);
    let text = String::from_utf8_lossy(&src[node.range().start..end]).to_string();
    let text = _strip_leading_annotations(&text);
    collapse_ws(&text)
        .trim_end_matches(&[' ', '{', ':', '='][..])
        .to_string()
}

fn _callable_signature<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let body = node.field("body");
    let end = body.map(|b| b.range().start).unwrap_or(node.range().end);
    let text = String::from_utf8_lossy(&src[node.range().start..end]).to_string();
    let text = _strip_leading_annotations(&text);
    let mut sig = collapse_ws(&text);
    if sig.ends_with('=') {
        sig = sig[..sig.len() - 1].trim_end().to_string();
    }
    sig.trim_end_matches(&[' ', '{', ';', '=', ':'][..])
        .to_string()
}

fn _property_signature<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> String {
    let text = _strip_leading_annotations(&node.text());
    collapse_ws(&text)
        .trim_end_matches(&[' ', '{', ';'][..])
        .to_string()
}

fn _extends_bases<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let ec = node.children().find(|c| c.kind() == "extends_clause");
    if let Some(e) = ec {
        for c in e.children() {
            if !c.is_named() {
                continue;
            }
            if c.kind() == "type_identifier"
                || c.kind() == "generic_type"
                || c.kind() == "compound_type"
            {
                out.push(collapse_ws(&c.text()));
            }
        }
    }
    out
}

fn _modifiers_node<'a, D: Doc>(node: &Node<'a, D>) -> Option<Node<'a, D>> {
    node.children().find(|c| c.kind() == "modifiers")
}

fn _visibility<'a, D: Doc>(node: &Node<'a, D>) -> String {
    if let Some(mods) = _modifiers_node(node) {
        for c in mods.children() {
            if c.kind() == "access_modifier" {
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

fn _annotations<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for c in node.children() {
        if c.kind() == "annotation" {
            out.push(collapse_ws(&c.text()));
        }
    }
    out
}

fn _scaladocs<'a, D: Doc>(node: &Node<'a, D>) -> Vec<String> {
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

fn _field_text<'a, D: Doc>(node: &Node<'a, D>, field_name: &str) -> Option<String> {
    node.field(field_name).map(|n| n.text().into_owned())
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
        "function_definition"
            | "function_declaration"
            | "class_definition"
            | "trait_definition"
            | "object_definition"
            | "lambda_expression"
            | "case_class_pattern"
    ) {
        return;
    }

    if kind == "call_expression" {
        if let Some(cs) = _call_site_from_call_scala(node, src, false) {
            out.push(cs);
        }
    } else if kind == "instance_expression" {
        if let Some(cs) = _call_site_from_instance(node, src) {
            out.push(cs);
        }
    } else if kind == "generic_function" {
        if let Some(cs) = _call_site_from_call_scala(node, src, false) {
            out.push(cs);
        }
    }

    for child in node.children() {
        _walk_calls_in_body(&child, src, out);
    }
}

fn _call_site_from_call_scala<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    is_construct: bool,
) -> Option<CallSite> {
    let func = node
        .field("function")
        .or_else(|| node.children().find(|c| c.is_named()))?;
    let (name, receiver) = _split_callee_scala(&func, src)?;
    let line = node.start_pos().line() as u32 + 1;
    let kind = if is_construct {
        CallKind::Construct
    } else {
        CallKind::Call
    };
    Some(CallSite {
        name,
        receiver,
        line,
        kind,
    })
}

fn _call_site_from_instance<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<CallSite> {
    // `new T(...)` — find the first identifier-like child after `new`.
    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        let k = c.kind();
        let k: &str = k.as_ref();
        if matches!(
            k,
            "type_identifier" | "generic_type" | "stable_type_identifier" | "identifier"
        ) {
            let raw = String::from_utf8_lossy(&src[c.range()]).to_string();
            let name = _last_type_segment_scala(&raw);
            if !name.is_empty() {
                let line = node.start_pos().line() as u32 + 1;
                return Some(CallSite {
                    name,
                    receiver: None,
                    line,
                    kind: CallKind::Construct,
                });
            }
        }
    }
    None
}

fn _split_callee_scala<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<(String, Option<String>)> {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "identifier" | "operator_identifier" => {
            Some((String::from_utf8_lossy(&src[node.range()]).to_string(), None))
        }
        "field_expression" => {
            let value = node.field("value");
            let field = node.field("field")?;
            let name = String::from_utf8_lossy(&src[field.range()]).to_string();
            let recv = value
                .map(|v| collapse_ws(&String::from_utf8_lossy(&src[v.range()])));
            Some((name, recv))
        }
        "generic_function" => {
            let func = node.field("function")?;
            _split_callee_scala(&func, src)
        }
        _ => {
            let raw = String::from_utf8_lossy(&src[node.range()]).to_string();
            let trimmed = raw.split('[').next().unwrap_or(&raw).trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some((trimmed, None))
            }
        }
    }
}

fn _last_type_segment_scala(text: &str) -> String {
    let head = text.split('[').next().unwrap_or(text).trim();
    head.rsplit('.').next().unwrap_or(head).trim().to_string()
}
