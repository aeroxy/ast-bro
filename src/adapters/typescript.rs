use super::base::{collapse_ws, count_parse_errors, field_text, LanguageAdapter};
use crate::core::{CallKind, CallSite, Declaration, DeclarationKind, ImportBinding, ParseResult};
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub struct TypeScriptAdapter;

impl LanguageAdapter for TypeScriptAdapter {
    fn language_name(&self) -> &'static str {
        "typescript"
    }

    fn parse<'a, D: Doc>(&self, path: &Path, source: &[u8], root: Node<'a, D>) -> ParseResult {
        let mut decls = Vec::new();
        _walk_module(&root, source, &mut decls);
        let mut imports = Vec::new();
        _walk_imports(&root, source, &mut imports);
        ParseResult {
            path: path.to_path_buf(),
            language: self.language_name(),
            source: source.to_vec(),
            line_count: source.iter().filter(|&&b| b == b'\n').count() + 1,
            declarations: decls,
            error_count: count_parse_errors(root.clone()),
            imports,
        }
    }
}

fn _walk_module<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<Declaration>) {
    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        if let Some(decl) = _node_to_decl(&child, src, false, false) {
            out.push(decl);
        }
    }
}

fn _node_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    _inside_class: bool,
    _inside_interface: bool,
) -> Option<Declaration> {
    let kind = node.kind();

    if kind == "export_statement" {
        let mut export_decorators = Vec::new();
        for c in node.children() {
            if c.kind() == "decorator" {
                export_decorators.push(collapse_ws(&c.text()));
            }
        }
        for inner in node.children() {
            if !inner.is_named() {
                continue;
            }
            // `export declare class/function/...` — the `declare` keyword
            // wraps the real decl in an `ambient_declaration` node. Step
            // through it so we still emit the inner decl.
            let effective = if inner.kind() == "ambient_declaration" {
                inner
                    .children()
                    .find(|c| c.is_named() && _is_handled_top_level(c.kind().as_ref()))
                    .unwrap_or_else(|| inner.clone())
            } else {
                inner.clone()
            };
            if _is_handled_top_level(effective.kind().as_ref()) {
                if let Some(mut decl) = _node_to_decl(&effective, src, _inside_class, _inside_interface) {
                    decl.start_byte = node.range().start;
                    decl.start_line = node.start_pos().line() + 1;
                    let ds_byte = _leading_doc_start_byte(node).unwrap_or(node.range().start);
                    decl.doc_start_byte = ds_byte;
                    decl.docs = _collect_docs(node);

                    if !export_decorators.is_empty() {
                        let mut new_attrs = export_decorators.clone();
                        new_attrs.extend(decl.attrs);
                        decl.attrs = new_attrs;
                    }

                    decl.signature = _signature_from_range(node, src, &effective);
                    return Some(decl);
                }
            }
        }
        return None;
    }

    if kind == "class_declaration" || kind == "abstract_class_declaration" {
        return Some(_class_to_decl(node, src));
    }
    if kind == "interface_declaration" {
        return Some(_interface_to_decl(node, src));
    }
    if kind == "enum_declaration" {
        return Some(_enum_to_decl(node, src));
    }
    if kind == "type_alias_declaration" {
        return Some(_type_alias_to_decl(node, src));
    }
    if kind == "function_declaration" || kind == "function_signature" {
        return Some(_function_to_decl(node, src, false));
    }

    if kind == "lexical_declaration" || kind == "variable_declaration" {
        return _lexical_to_decl(node, src);
    }

    if kind == "method_definition" {
        return _method_to_decl(node, src);
    }
    if kind == "public_field_definition" {
        return _class_field_to_decl(node, src);
    }

    if kind == "property_signature" {
        return _property_signature_to_decl(node, src);
    }
    if kind == "method_signature" || kind == "construct_signature" || kind == "call_signature" {
        return _method_signature_to_decl(node, src);
    }
    if kind == "index_signature" {
        return None;
    }

    if kind == "property_identifier" || kind == "enum_assignment" {
        return _enum_member_to_decl(node, src);
    }

    None
}

fn _is_handled_top_level(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "type_alias_declaration"
            | "function_declaration"
            | "function_signature"
            | "lexical_declaration"
            | "variable_declaration"
    )
}

fn _class_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let bases = _class_bases(node, src);
    let attrs = _decorators(node, src);
    let docs = _collect_docs(node);

    let signature = _class_signature(node, src);

    let mut children = Vec::new();
    if let Some(body) = node.field("body") {
        for c in body.children() {
            if !c.is_named() {
                continue;
            }
            if let Some(d) = _node_to_decl(&c, src, true, false) {
                children.push(d);
            }
        }
    }

    let range = node.range();
    Declaration {
        kind: DeclarationKind::Class,
        name,
        signature,
        bases,
        attrs,
        docs,
        docs_inside: false,
        visibility: "public".to_string(),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: _leading_doc_start_byte(node).unwrap_or(range.start),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children,
        calls: Vec::new(),
    }
}

fn _interface_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let bases = _interface_bases(node, src);
    let docs = _collect_docs(node);

    let body = node.field("body");
    let mut children = Vec::new();
    if let Some(b) = &body {
        for c in b.children() {
            if !c.is_named() {
                continue;
            }
            if let Some(d) = _node_to_decl(&c, src, false, true) {
                children.push(d);
            }
        }
    }

    let signature = _head_text(node, src, body.as_ref());
    let range = node.range();
    Declaration {
        kind: DeclarationKind::Interface,
        name,
        signature,
        bases,
        attrs: Vec::new(),
        docs,
        docs_inside: false,
        visibility: "public".to_string(),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: _leading_doc_start_byte(node).unwrap_or(range.start),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children,
        calls: Vec::new(),
    }
}

fn _enum_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let docs = _collect_docs(node);
    let body = node.field("body");

    let mut children = Vec::new();
    if let Some(b) = &body {
        for c in b.children() {
            if !c.is_named() {
                continue;
            }
            if let Some(d) = _node_to_decl(&c, src, false, false) {
                children.push(d);
            }
        }
    }

    let signature = _head_text(node, src, body.as_ref());
    let range = node.range();
    Declaration {
        kind: DeclarationKind::Enum,
        name,
        signature,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs,
        docs_inside: false,
        visibility: "public".to_string(),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: _leading_doc_start_byte(node).unwrap_or(range.start),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children,
        calls: Vec::new(),
    }
}

fn _enum_member_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<Declaration> {
    let name = if node.kind() == "enum_assignment" {
        node.field("name")
            .or_else(|| node.children().find(|c| c.is_named()))
            .map(|n| n.text().into_owned())
    } else {
        Some(node.text().into_owned())
    }?;

    let sig = collapse_ws(&node.text());
    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::EnumMember,
        name,
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
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

fn _type_alias_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let sig = collapse_ws(&node.text()).trim_end_matches(';').to_string();
    let range = node.range();
    Declaration {
        kind: DeclarationKind::Field,
        name,
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: _collect_docs(node),
        docs_inside: false,
        visibility: "public".to_string(),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: _leading_doc_start_byte(node).unwrap_or(range.start),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    }
}

fn _function_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    inside_class: bool,
) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let sig = _function_signature(node, src);
    let docs = _collect_docs(node);

    let kind = if inside_class {
        DeclarationKind::Method
    } else {
        DeclarationKind::Function
    };
    let visibility = _visibility_for_name(&name);

    let range = node.range();
    let mut calls = Vec::new();
    if let Some(body) = node.field("body") {
        _walk_calls_in_body(&body, src, &mut calls);
    }
    Declaration {
        kind,
        name,
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs,
        docs_inside: false,
        visibility,
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: _leading_doc_start_byte(node).unwrap_or(range.start),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls,
    }
}

fn _method_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name")?;
    let kind = if name == "constructor" {
        DeclarationKind::Constructor
    } else {
        DeclarationKind::Method
    };
    let sig = _function_signature(node, src);
    let docs = _collect_docs(node);

    let mut visibility = _visibility_from_modifiers(node, src);
    if visibility.is_empty() {
        visibility = _visibility_for_name(&name);
    }
    if visibility.is_empty() {
        visibility = "public".to_string();
    }

    let attrs = _decorators(node, src);
    let range = node.range();
    let mut calls = Vec::new();
    if let Some(body) = node.field("body") {
        _walk_calls_in_body(&body, src, &mut calls);
    }
    Some(Declaration {
        kind,
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
        doc_start_byte: _leading_doc_start_byte(node).unwrap_or(range.start),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls,
    })
}

fn _method_signature_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name")?;
    let sig = collapse_ws(&node.text()).trim_end_matches(';').to_string();
    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Method,
        name,
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: _collect_docs(node),
        docs_inside: false,
        visibility: "public".to_string(),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: _leading_doc_start_byte(node).unwrap_or(range.start),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _class_field_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name")?;
    let mut sig = collapse_ws(&node.text());
    if let Some(idx) = sig.find(" = ") {
        sig = sig[..idx].to_string();
    }
    sig = sig.trim_end_matches(';').to_string();

    let mut visibility = _visibility_from_modifiers(node, src);
    if visibility.is_empty() {
        visibility = _visibility_for_name(&name);
    }
    if visibility.is_empty() {
        visibility = "public".to_string();
    }

    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Field,
        name,
        signature: sig,
        bases: Vec::new(),
        attrs: _decorators(node, src),
        docs: _collect_docs(node),
        docs_inside: false,
        visibility,
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: _leading_doc_start_byte(node).unwrap_or(range.start),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _property_signature_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name")?;
    let sig = collapse_ws(&node.text())
        .trim_end_matches(&[';', ','][..])
        .to_string();
    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Field,
        name,
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: _collect_docs(node),
        docs_inside: false,
        visibility: "public".to_string(),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: _leading_doc_start_byte(node).unwrap_or(range.start),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _lexical_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let mut declarators = node
        .children()
        .filter(|c| c.kind() == "variable_declarator");
    let d = declarators.next()?;
    let name_node = d.field("name")?;
    if name_node.kind() != "identifier" {
        return None;
    }
    let name = name_node.text().into_owned();
    let docs = _collect_docs(node);
    let value = d.field("value");

    if let Some(v) = &value {
        if matches!(
            v.kind().as_ref(),
            "arrow_function" | "function_expression" | "function"
        ) {
            let body = v.field("body");
            let end = body.as_ref().map(|b| b.range().start).unwrap_or(v.range().end);
            let text = String::from_utf8_lossy(&src[node.range().start..end]).to_string();
            let sig = collapse_ws(&text).trim_end_matches('{').trim().to_string();

            let mut calls = Vec::new();
            if let Some(body) = &body {
                _walk_calls_in_body(body, src, &mut calls);
            }

            let range = node.range();
            return Some(Declaration {
                kind: DeclarationKind::Function,
                name: name.clone(),
                signature: sig,
                bases: Vec::new(),
                attrs: Vec::new(),
                docs,
                docs_inside: false,
                visibility: _visibility_for_name(&name),
                start_line: node.start_pos().line() + 1,
                end_line: node.end_pos().line() + 1,
                start_byte: range.start,
                end_byte: range.end,
                doc_start_byte: _leading_doc_start_byte(node).unwrap_or(range.start),
                native_kind: None,
                modifiers: Vec::new(),
                deprecated: false,
                children: Vec::new(),
                calls,
            });
        }
    }

    let sig = collapse_ws(&node.text()).trim_end_matches(';').to_string();
    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Field,
        name: name.clone(),
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs,
        docs_inside: false,
        visibility: _visibility_for_name(&name),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: _leading_doc_start_byte(node).unwrap_or(range.start),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _function_signature<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let body = node.field("body");
    let end = body.map(|b| b.range().start).unwrap_or(node.range().end);
    let text = String::from_utf8_lossy(&src[node.range().start..end]).to_string();
    let text = _strip_leading_decorators(&text);
    collapse_ws(&text)
        .trim_end_matches(&[' ', '{', ';'][..])
        .to_string()
}

fn _class_signature<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let body = node.field("body");
    let end = body.map(|b| b.range().start).unwrap_or(node.range().end);
    let text = String::from_utf8_lossy(&src[node.range().start..end]).to_string();
    let text = _strip_leading_decorators(&text);
    collapse_ws(&text)
        .trim_end_matches(&[' ', '{'][..])
        .to_string()
}

fn _head_text<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], body: Option<&Node<'a, D>>) -> String {
    let end = body.map(|b| b.range().start).unwrap_or(node.range().end);
    let text = String::from_utf8_lossy(&src[node.range().start..end]).to_string();
    collapse_ws(&text)
        .trim_end_matches(&[' ', '{'][..])
        .to_string()
}

fn _strip_leading_decorators(text: &str) -> String {
    let mut s = text.trim_start();
    while s.starts_with('@') {
        if let Some(nl) = s.find('\n') {
            s = s[nl + 1..].trim_start();
        } else {
            break;
        }
    }
    s.to_string()
}

fn _class_bases<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for child in node.children() {
        if child.kind() == "class_heritage" {
            for h in child.children() {
                if !h.is_named() {
                    continue;
                }
                for inner in h.children() {
                    if !inner.is_named() {
                        continue;
                    }
                    let t = collapse_ws(&inner.text()).trim_end_matches(',').to_string();
                    if !t.is_empty() {
                        out.push(t);
                    }
                }
            }
        }
    }
    out
}

fn _interface_bases<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for child in node.children() {
        if child.kind() == "extends_type_clause" {
            for inner in child.children() {
                if !inner.is_named() {
                    continue;
                }
                let t = collapse_ws(&inner.text()).trim_end_matches(',').to_string();
                if !t.is_empty() {
                    out.push(t);
                }
            }
        }
    }
    out
}

fn _decorators<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut preceding = Vec::new();

    let mut sib = node.prev();
    while let Some(s) = sib {
        if s.kind() == "decorator" {
            preceding.push(collapse_ws(&s.text()));
            sib = s.prev();
        } else {
            break;
        }
    }
    preceding.reverse();

    for c in node.children() {
        if c.kind() == "decorator" {
            out.push(collapse_ws(&c.text()));
        }
    }

    preceding.extend(out);
    preceding
}

fn _signature_from_range<'a, D: Doc>(
    outer: &Node<'a, D>,
    src: &[u8],
    inner: &Node<'a, D>,
) -> String {
    let body = inner.field("body");
    let end = body.map(|b| b.range().start).unwrap_or(inner.range().end);
    let text = String::from_utf8_lossy(&src[outer.range().start..end]).to_string();
    let text = _strip_leading_decorators(&text);
    collapse_ws(&text)
        .trim_end_matches(&[' ', '{', ';'][..])
        .to_string()
}

fn _visibility_from_modifiers<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> String {
    for c in node.children() {
        if c.kind() == "accessibility_modifier" {
            let t = c.text().trim().to_string();
            if t == "public" || t == "protected" || t == "private" {
                return t;
            }
        }
    }
    if let Some(name_node) = node.field("name") {
        if name_node.kind() == "private_property_identifier" {
            return "private".to_string();
        }
    }
    String::new()
}

fn _visibility_for_name(name: &str) -> String {
    if name.starts_with('_') {
        "private".to_string()
    } else {
        String::new()
    }
}

fn _collect_docs<'a, D: Doc>(node: &Node<'a, D>) -> Vec<String> {
    let mut docs = Vec::new();
    let mut sib = node.prev();
    while let Some(s) = sib {
        if s.kind() == "comment" {
            docs.push(s.text().into_owned());
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
        if s.kind() == "comment" {
            first = Some(s.clone());
            sib = s.prev();
        } else {
            break;
        }
    }
    first.map(|f| f.range().start)
}

// ---------------------------------------------------------------------------
// Call-site extraction
// ---------------------------------------------------------------------------

fn _walk_calls_in_body<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<CallSite>) {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    // Stop at *named* nested scopes (they are — or should be — their own graph
    // nodes), but descend *through* anonymous closures so their calls are
    // attributed to the enclosing declaration. This is what makes curried
    // arrows (`a => b => f()`), returned closures, and inline callbacks
    // (`.map(x => f(x))`) show up in the call graph. Anonymous closures are
    // never extracted as separate declarations, so there is no double-count.
    //
    // `arrow_function` and `function_expression` are intentionally NOT in this
    // list — see _walk_calls_in_body's callers, which pass a body node, so the
    // root of a walk is never one of these for its own declaration.
    if matches!(
        kind,
        "function_declaration"
            | "function"
            | "method_definition"
            | "class_declaration"
            | "class"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration"
            | "module_declaration"
    ) {
        return;
    }

    if kind == "call_expression" {
        if let Some(cs) = _call_site_from_call(node, src, false) {
            out.push(cs);
        }
    } else if kind == "new_expression" {
        if let Some(cs) = _call_site_from_call(node, src, true) {
            out.push(cs);
        }
    }

    for child in node.children() {
        _walk_calls_in_body(&child, src, out);
    }
}

fn _call_site_from_call<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], is_construct: bool) -> Option<CallSite> {
    // tree-sitter-typescript exposes the callee as the `function` field on
    // call_expression and `constructor` on new_expression.
    let func = node.field("function").or_else(|| node.field("constructor"))?;
    let (name, receiver) = _extract_callee_name_ts(&func, src)?;
    let line = node.start_pos().line() as u32 + 1;
    let kind = if is_construct { CallKind::Construct } else { CallKind::Call };
    Some(CallSite { name, receiver, line, kind })
}

fn _extract_callee_name_ts<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<(String, Option<String>)> {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "identifier" | "property_identifier" => {
            Some((String::from_utf8_lossy(&src[node.range()]).to_string(), None))
        }
        "member_expression" => {
            let object = node.field("object");
            let property = node.field("property")?;
            let name = String::from_utf8_lossy(&src[property.range()]).to_string();
            let recv = object.map(|o| String::from_utf8_lossy(&src[o.range()]).to_string());
            Some((name, recv))
        }
        _ => Some((String::from_utf8_lossy(&src[node.range()]).to_string(), None)),
    }
}

// ---------------------------------------------------------------------------
// Import extraction
//
// Handles:
//   import foo from './x';
//   import { a, b as c } from './x';
//   import * as ns from './x';
//   import './side-effect';
//   const x = require('./y');               (CommonJS — best-effort)
// ---------------------------------------------------------------------------

fn _walk_imports<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<ImportBinding>) {
    for child in node.children() {
        let kind = child.kind();
        let kind: &str = kind.as_ref();
        let line = child.start_pos().line() as u32 + 1;
        if kind == "import_statement" {
            _handle_import_stmt(&child, src, line, out);
        }
    }
}

fn _handle_import_stmt<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    line: u32,
    out: &mut Vec<ImportBinding>,
) {
    // The `source` field carries the module specifier as a string literal —
    // strip the surrounding quotes.
    let source = match node.field("source") {
        Some(s) => _unquote(&String::from_utf8_lossy(&src[s.range()])),
        None => return,
    };

    // The `import_clause` (if present) describes which bindings get pulled
    // in. It can hold:
    //   - identifier (default import: `import Foo from ...`)
    //   - namespace_import (`import * as ns from ...`)
    //   - named_imports (`import { a, b as c } from ...`)
    let mut found_clause = false;
    for child in node.children() {
        let k = child.kind();
        let k: &str = k.as_ref();
        if k == "import_clause" {
            found_clause = true;
            for inner in child.children() {
                let ik = inner.kind();
                let ik: &str = ik.as_ref();
                match ik {
                    "identifier" => {
                        let local = String::from_utf8_lossy(&src[inner.range()]).to_string();
                        out.push(ImportBinding { local, module: source.clone(), line });
                    }
                    "namespace_import" => {
                        // namespace_import has children including `* as ident`
                        for ns in inner.children() {
                            if matches!(ns.kind().as_ref(), "identifier") {
                                let local = String::from_utf8_lossy(&src[ns.range()]).to_string();
                                out.push(ImportBinding { local, module: source.clone(), line });
                            }
                        }
                    }
                    "named_imports" => {
                        for spec in inner.children() {
                            let sk = spec.kind();
                            let sk: &str = sk.as_ref();
                            if sk == "import_specifier" {
                                let name = spec.field("name");
                                let alias = spec.field("alias");
                                let local = alias
                                    .as_ref()
                                    .or(name.as_ref())
                                    .map(|n| String::from_utf8_lossy(&src[n.range()]).to_string());
                                if let Some(local) = local {
                                    out.push(ImportBinding {
                                        local,
                                        module: source.clone(),
                                        line,
                                    });
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if !found_clause {
        // `import './side-effect'` — no binding to record, but we still
        // emit a synthetic entry so resolvers know the file references it.
        // (Local name = empty — the resolver will ignore empty-local entries.)
        let _ = source;
    }
}

fn _unquote(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('\'').or_else(|| s.strip_prefix('"')).unwrap_or(s);
    let s = s.strip_suffix('\'').or_else(|| s.strip_suffix('"')).unwrap_or(s);
    s.to_string()
}
