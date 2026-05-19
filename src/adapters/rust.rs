use super::base::{collapse_ws, count_parse_errors, field_text, LanguageAdapter};
use crate::core::{CallKind, CallSite, Declaration, DeclarationKind, ImportBinding, ParseResult};
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub struct RustAdapter;

impl LanguageAdapter for RustAdapter {
    fn language_name(&self) -> &'static str {
        "rust"
    }

    fn parse<'a, D: Doc>(&self, path: &Path, source: &[u8], root: Node<'a, D>) -> ParseResult {
        let mut decls = Vec::new();
        _walk_mod(&root, source, &mut decls);
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

/// Walk a module (or the file root) in two passes:
/// 1. Emit every top-level decl as today, EXCEPT `impl_item` which is
///    held aside in `pending_impls`.
/// 2. Distribute each pending impl into its target type's `bases` /
///    `children`. Impls whose target isn't declared in this scope (e.g.
///    `impl Display for Foo` where Foo lives in another crate) fall
///    through as a synthesized top-level decl, matching the pre-rewrite
///    behaviour so we never lose info.
///
/// `ast-bro implements Trait` now finds the *struct*, not a
/// synthetic `impl_Foo` shadow.
fn _walk_mod<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<Declaration>) {
    let mut pending_impls: Vec<Declaration> = Vec::new();

    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        if child.kind() == "impl_item" {
            pending_impls.push(_impl_to_decl(&child, src));
        } else if let Some(decl) = _node_to_decl(&child, src) {
            out.push(decl);
        }
    }

    for impl_decl in pending_impls {
        // `_impl_to_decl` synthesises a name like `impl_Foo`; the real
        // target is the suffix.
        let target_name = impl_decl
            .name
            .strip_prefix("impl_")
            .unwrap_or(&impl_decl.name)
            .to_string();

        if let Some(target) = out
            .iter_mut()
            .find(|d| d.name == target_name && _is_regroup_target(&d.kind))
        {
            // Trait impl: lift the trait into the target's `bases` so
            // `find_implementations` traverses Foo, not impl_Foo.
            for b in impl_decl.bases {
                if !target.bases.contains(&b) {
                    target.bases.push(b);
                }
            }
            // Inherent or trait impl: methods become members of the type.
            target.children.extend(impl_decl.children);
        } else {
            // Target type lives elsewhere (cross-crate / foreign type).
            // Keep the synthesized decl so the methods aren't lost.
            out.push(impl_decl);
        }
    }
}

fn _is_regroup_target(kind: &DeclarationKind) -> bool {
    matches!(
        kind,
        DeclarationKind::Struct
            | DeclarationKind::Enum
            | DeclarationKind::Interface
            | DeclarationKind::Class
            | DeclarationKind::Record
    )
}

fn _node_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let kind = node.kind();

    if kind == "struct_item" {
        return Some(_struct_to_decl(node, src));
    }
    if kind == "enum_item" {
        return Some(_enum_to_decl(node, src));
    }
    if kind == "trait_item" {
        return Some(_trait_to_decl(node, src));
    }
    if kind == "function_item" {
        return Some(_function_to_decl(node, src, false));
    }
    if kind == "mod_item" {
        return Some(_mod_to_decl(node, src));
    }
    if kind == "macro_definition" {
        return Some(_macro_to_decl(node, src));
    }
    if kind == "foreign_mod_item" {
        return Some(_foreign_mod_to_decl(node, src));
    }
    if kind == "union_item" {
        // Treated as a struct for outline purposes — same shape as far
        // as users navigating an outline care.
        return Some(_struct_to_decl(node, src));
    }

    None
}

fn _struct_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());

    let mut attrs = Vec::new();
    let mut docs = Vec::new();
    _extract_attrs_and_docs(node, src, &mut attrs, &mut docs);

    let mut children = Vec::new();
    if let Some(body) = node.field("body") {
        match body.kind().as_ref() {
            "field_declaration_list" => {
                for field in body.children() {
                    if field.kind() == "field_declaration" {
                        if let Some(fd) = _field_to_decl(&field, src) {
                            children.push(fd);
                        }
                    }
                }
            }
            "ordered_field_declaration_list" => {
                // Tuple struct: tree-sitter renders the body as a flat
                // sequence of `visibility_modifier?` + type nodes (no
                // `field_declaration` wrapper). Track the running visibility
                // and emit one Field per type, with synthetic name "0", "1",…
                // so users can navigate `pair.0` style.
                let mut pending_vis = String::new();
                let mut pending_attrs: Vec<String> = Vec::new();
                let mut idx = 0usize;
                for c in body.children() {
                    if !c.is_named() {
                        continue;
                    }
                    let k = c.kind();
                    if k == "visibility_modifier" {
                        pending_vis = collapse_ws(&c.text());
                        continue;
                    }
                    if k == "attribute_item" {
                        pending_attrs.push(collapse_ws(&c.text()));
                        continue;
                    }
                    children.push(_positional_field_to_decl(
                        &c,
                        src,
                        idx,
                        std::mem::take(&mut pending_vis),
                        std::mem::take(&mut pending_attrs),
                    ));
                    idx += 1;
                }
            }
            _ => {}
        }
    }
    // Unit structs (`struct Foo;`) have no body field — children stays empty,
    // which is the correct outline.

    let sig_end = node
        .field("body")
        .map(|b| b.range().start)
        .unwrap_or(node.range().end);
    let sig = collapse_ws(&String::from_utf8_lossy(&src[node.range().start..sig_end]))
        .trim_end_matches(&[' ', '{', ';'][..])
        .to_string();

    Declaration {
        kind: DeclarationKind::Struct,
        name,
        signature: sig,
        bases: Vec::new(),
        deprecated: false,
        attrs,
        docs,
        docs_inside: false,
        visibility: _visibility(node, src),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: node.range().start,
        end_byte: node.range().end,
        doc_start_byte: _doc_start(node),
        native_kind: None,
        modifiers: Vec::new(),
        children,
        calls: Vec::new(),
    }
}

fn _enum_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());

    let mut attrs = Vec::new();
    let mut docs = Vec::new();
    _extract_attrs_and_docs(node, src, &mut attrs, &mut docs);

    let mut children = Vec::new();
    if let Some(body) = node.field("body") {
        for variant in body.children() {
            if variant.kind() == "enum_variant" {
                let vname = field_text(&variant, "name").unwrap_or_else(|| "?".to_string());
                let vr = variant.range();
                children.push(Declaration {
                    kind: DeclarationKind::EnumMember,
                    name: vname.clone(),
                    signature: vname,
                    bases: Vec::new(),
                    attrs: Vec::new(),
                    docs: Vec::new(),
                    docs_inside: false,
                    visibility: String::new(),
                    start_line: variant.start_pos().line() + 1,
                    end_line: variant.end_pos().line() + 1,
                    start_byte: vr.start,
                    end_byte: vr.end,
                    doc_start_byte: vr.start,
                    native_kind: None,
                    modifiers: Vec::new(),
                    deprecated: false,
                    children: Vec::new(),
                    calls: Vec::new(),
                });
            }
        }
    }

    let sig_end = node
        .field("body")
        .map(|b| b.range().start)
        .unwrap_or(node.range().end);
    let sig = collapse_ws(&String::from_utf8_lossy(&src[node.range().start..sig_end]))
        .trim_end_matches(&[' ', '{'][..])
        .to_string();

    Declaration {
        kind: DeclarationKind::Enum,
        name,
        signature: sig,
        bases: Vec::new(),
        deprecated: false,
        attrs,
        docs,
        docs_inside: false,
        visibility: _visibility(node, src),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: node.range().start,
        end_byte: node.range().end,
        doc_start_byte: _doc_start(node),
        native_kind: None,
        modifiers: Vec::new(),
        children,
        calls: Vec::new(),
    }
}

fn _trait_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());

    let mut attrs = Vec::new();
    let mut docs = Vec::new();
    _extract_attrs_and_docs(node, src, &mut attrs, &mut docs);

    let mut children = Vec::new();
    if let Some(body) = node.field("body") {
        for item in body.children() {
            match item.kind().as_ref() {
                "function_signature_item" | "function_item" => {
                    children.push(_function_to_decl(&item, src, true));
                }
                "associated_type" => {
                    if let Some(d) = _associated_type_to_decl(&item, src) {
                        children.push(d);
                    }
                }
                "const_item" => {
                    if let Some(d) = _const_or_static_to_field(&item, src) {
                        children.push(d);
                    }
                }
                _ => {}
            }
        }
    }

    let sig_end = node
        .field("body")
        .map(|b| b.range().start)
        .unwrap_or(node.range().end);
    let sig = collapse_ws(&String::from_utf8_lossy(&src[node.range().start..sig_end]))
        .trim_end_matches(&[' ', '{'][..])
        .to_string();

    Declaration {
        kind: DeclarationKind::Interface,
        name,
        signature: sig,
        bases: Vec::new(),
        deprecated: false,
        attrs,
        docs,
        docs_inside: false,
        visibility: _visibility(node, src),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: node.range().start,
        end_byte: node.range().end,
        doc_start_byte: _doc_start(node),
        native_kind: None,
        modifiers: Vec::new(),
        children,
        calls: Vec::new(),
    }
}

fn _impl_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let name = field_text(node, "type").unwrap_or_else(|| "?".to_string());
    let trait_node = node.field("trait");
    let trait_name = trait_node.map(|t| collapse_ws(&t.text()));

    let mut attrs = Vec::new();
    let mut docs = Vec::new();
    _extract_attrs_and_docs(node, src, &mut attrs, &mut docs);

    let mut children = Vec::new();
    if let Some(body) = node.field("body") {
        for item in body.children() {
            if item.kind() == "function_item" {
                children.push(_function_to_decl(&item, src, true));
            }
        }
    }

    let mut sig = "impl ".to_string();
    if let Some(t) = &trait_name {
        sig.push_str(t);
        sig.push_str(" for ");
    }
    sig.push_str(&name);

    Declaration {
        kind: DeclarationKind::Class,
        name: format!("impl_{}", name),
        signature: sig,
        bases: trait_name.into_iter().collect(),
        deprecated: false,
        attrs,
        docs,
        docs_inside: false,
        visibility: String::new(),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: node.range().start,
        end_byte: node.range().end,
        doc_start_byte: _doc_start(node),
        native_kind: None,
        modifiers: Vec::new(),
        children,
        calls: Vec::new(),
    }
}

fn _function_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], is_method: bool) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());

    let mut attrs = Vec::new();
    let mut docs = Vec::new();
    _extract_attrs_and_docs(node, src, &mut attrs, &mut docs);

    let sig_end = node
        .field("body")
        .map(|b| b.range().start)
        .unwrap_or(node.range().end);
    let sig = collapse_ws(&String::from_utf8_lossy(&src[node.range().start..sig_end]))
        .trim_end_matches(&[' ', '{', ';'][..])
        .to_string();

    let mut calls = Vec::new();
    if let Some(body) = node.field("body") {
        _walk_calls_in_body(&body, src, &mut calls);
    }

    Declaration {
        kind: if is_method {
            DeclarationKind::Method
        } else {
            DeclarationKind::Function
        },
        name,
        signature: sig,
        bases: Vec::new(),
        deprecated: false,
        attrs,
        docs,
        docs_inside: false,
        visibility: _visibility(node, src),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: node.range().start,
        end_byte: node.range().end,
        doc_start_byte: _doc_start(node),
        native_kind: None,
        modifiers: Vec::new(),
        children: Vec::new(),
        calls,
    }
}

// ---------------------------------------------------------------------------
// Call-site extraction
//
// Walks every descendant of a function body, emitting one `CallSite` per
// call/macro invocation. Nested function-like decls (`function_item`,
// `closure_expression`, `impl_item`, etc.) get their own `Declaration::calls`
// so we stop descending into them here.
// ---------------------------------------------------------------------------

fn _walk_calls_in_body<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<CallSite>) {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    if matches!(
        kind,
        "function_item"
            | "closure_expression"
            | "impl_item"
            | "trait_item"
            | "struct_item"
            | "enum_item"
            | "mod_item"
            | "macro_definition"
    ) {
        // Owned by another Declaration; skip recursion.
        return;
    }

    if kind == "call_expression" {
        if let Some(cs) = _call_site_from_call(node, src) {
            out.push(cs);
        }
    } else if kind == "macro_invocation" {
        if let Some(cs) = _call_site_from_macro(node, src) {
            out.push(cs);
        }
    }

    for child in node.children() {
        _walk_calls_in_body(&child, src, out);
    }
}

fn _call_site_from_call<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<CallSite> {
    let func = node.field("function")?;
    let (name, receiver, kind) = _extract_callee_name(&func, src)?;
    Some(CallSite {
        name,
        receiver,
        line: node.start_pos().line() as u32 + 1,
        kind,
    })
}

fn _call_site_from_macro<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<CallSite> {
    let macro_name = node.field("macro")?;
    let (name, receiver, _) = _extract_callee_name(&macro_name, src)?;
    Some(CallSite {
        name,
        receiver,
        line: node.start_pos().line() as u32 + 1,
        kind: CallKind::Macro,
    })
}

fn _extract_callee_name<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<(String, Option<String>, CallKind)> {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "identifier" => {
            let name = String::from_utf8_lossy(&src[node.range()]).to_string();
            Some((name, None, CallKind::Call))
        }
        "field_expression" => {
            // obj.method — `value` field is the receiver, `field` is the name
            let value = node.field("value")?;
            let field = node.field("field")?;
            let name = String::from_utf8_lossy(&src[field.range()]).to_string();
            let recv = String::from_utf8_lossy(&src[value.range()]).to_string();
            Some((name, Some(recv), CallKind::Call))
        }
        "scoped_identifier" => {
            // path::name() — last segment is the name, preceding path is the receiver.
            let path = node.field("path");
            let name_node = node.field("name")?;
            let name = String::from_utf8_lossy(&src[name_node.range()]).to_string();
            let recv = path.map(|p| String::from_utf8_lossy(&src[p.range()]).to_string());
            let kind = if name == "new" {
                CallKind::Construct
            } else if recv.as_deref() == Some("super") {
                CallKind::Super
            } else {
                CallKind::Call
            };
            Some((name, recv, kind))
        }
        "generic_function" => {
            // foo::<T>() — function field is the inner identifier or path.
            let func = node.field("function")?;
            _extract_callee_name(&func, src)
        }
        _ => {
            // Fall back to the last identifier-like token.
            let txt = String::from_utf8_lossy(&src[node.range()]).to_string();
            Some((txt, None, CallKind::Call))
        }
    }
}

// ---------------------------------------------------------------------------
// Import extraction
//
// Walks the file root for `use_declaration` nodes, expanding grouped uses
// (`use foo::{a, b as c}`) and renames into per-name `ImportBinding`s.
// ---------------------------------------------------------------------------

fn _walk_imports<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<ImportBinding>) {
    for child in node.children() {
        let kind = child.kind();
        let kind: &str = kind.as_ref();
        if kind == "use_declaration" {
            if let Some(arg) = child.field("argument") {
                let line = child.start_pos().line() as u32 + 1;
                _expand_use_tree(&arg, src, &Vec::new(), line, out);
            }
        }
    }
}

fn _expand_use_tree<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    prefix: &Vec<String>,
    line: u32,
    out: &mut Vec<ImportBinding>,
) {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "identifier" | "self" | "super" | "crate" => {
            let name = String::from_utf8_lossy(&src[node.range()]).to_string();
            let mut full = prefix.clone();
            full.push(name.clone());
            out.push(ImportBinding {
                local: name,
                module: full.join("::"),
                line,
            });
        }
        "scoped_identifier" => {
            // path = nested scoped_identifier or identifier; name = trailing identifier
            let mut full = prefix.clone();
            if let Some(path) = node.field("path") {
                _collect_scope_segments(&path, src, &mut full);
            }
            if let Some(name_node) = node.field("name") {
                let local = String::from_utf8_lossy(&src[name_node.range()]).to_string();
                full.push(local.clone());
                out.push(ImportBinding {
                    local,
                    module: full.join("::"),
                    line,
                });
            }
        }
        "use_as_clause" => {
            // path AS alias
            let path = node.field("path");
            let alias = node.field("alias");
            let mut full = prefix.clone();
            if let Some(path) = path {
                let mut segs: Vec<String> = Vec::new();
                _collect_scope_segments(&path, src, &mut segs);
                full.extend(segs);
            }
            let module = full.join("::");
            let local = alias
                .map(|a| String::from_utf8_lossy(&src[a.range()]).to_string())
                .unwrap_or_else(|| {
                    full.last().cloned().unwrap_or_default()
                });
            out.push(ImportBinding { local, module, line });
        }
        "use_list" | "scoped_use_list" => {
            let mut new_prefix = prefix.clone();
            if let Some(path) = node.field("path") {
                _collect_scope_segments(&path, src, &mut new_prefix);
            }
            for child in node.children() {
                let ck = child.kind();
                let ck: &str = ck.as_ref();
                if matches!(ck, "{" | "}" | ",") {
                    continue;
                }
                _expand_use_tree(&child, src, &new_prefix, line, out);
            }
        }
        "scoped_use_tree" => {
            let mut new_prefix = prefix.clone();
            if let Some(path) = node.field("path") {
                _collect_scope_segments(&path, src, &mut new_prefix);
            }
            if let Some(list) = node.field("list") {
                _expand_use_tree(&list, src, &new_prefix, line, out);
            }
        }
        "use_wildcard" => { /* `use foo::*` — no specific binding to record */ }
        _ => {
            // Recurse into any remaining structures we don't recognize so we
            // don't silently drop bindings on grammar shape changes.
            for child in node.children() {
                _expand_use_tree(&child, src, prefix, line, out);
            }
        }
    }
}

fn _collect_scope_segments<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<String>) {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "identifier" | "self" | "super" | "crate" => {
            out.push(String::from_utf8_lossy(&src[node.range()]).to_string());
        }
        "scoped_identifier" => {
            if let Some(path) = node.field("path") {
                _collect_scope_segments(&path, src, out);
            }
            if let Some(name) = node.field("name") {
                out.push(String::from_utf8_lossy(&src[name.range()]).to_string());
            }
        }
        _ => {
            // Unknown; bail with raw text as fallback so resolution at least sees something.
            out.push(String::from_utf8_lossy(&src[node.range()]).to_string());
        }
    }
}

fn _mod_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());

    let mut attrs = Vec::new();
    let mut docs = Vec::new();
    _extract_attrs_and_docs(node, src, &mut attrs, &mut docs);

    let mut children = Vec::new();
    if let Some(body) = node.field("body") {
        _walk_mod(&body, src, &mut children);
    }

    let sig_end = node
        .field("body")
        .map(|b| b.range().start)
        .unwrap_or(node.range().end);
    let sig = collapse_ws(&String::from_utf8_lossy(&src[node.range().start..sig_end]))
        .trim_end_matches(&[' ', '{', ';'][..])
        .to_string();

    Declaration {
        kind: DeclarationKind::Namespace,
        name: name.clone(),
        signature: sig,
        bases: Vec::new(),
        deprecated: false,
        attrs,
        docs,
        docs_inside: false,
        visibility: _visibility(node, src),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: node.range().start,
        end_byte: node.range().end,
        doc_start_byte: _doc_start(node),
        native_kind: None,
        modifiers: Vec::new(),
        children,
        calls: Vec::new(),
    }
}

fn _macro_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());

    let mut attrs = Vec::new();
    let mut docs = Vec::new();
    _extract_attrs_and_docs(node, src, &mut attrs, &mut docs);

    let visibility = if attrs.iter().any(|a| a.contains("macro_export")) {
        "pub".to_string()
    } else {
        String::new()
    };

    let sig = format!("macro_rules! {}", name);

    Declaration {
        kind: DeclarationKind::Delegate,
        name,
        signature: sig,
        bases: Vec::new(),
        deprecated: false,
        attrs,
        docs,
        docs_inside: false,
        visibility,
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: node.range().start,
        end_byte: node.range().end,
        doc_start_byte: _doc_start(node),
        native_kind: None,
        modifiers: Vec::new(),
        children: Vec::new(),
        calls: Vec::new(),
    }
}

/// `extern "C" { fn foo(...); static BAR: T; }` — surface the FFI block
/// as a Namespace named after the ABI string, with each foreign item as
/// a child function/field.
fn _foreign_mod_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    // `extern_modifier` is the `extern "C"` (or `extern "system"`, …) prefix.
    let abi = node
        .children()
        .find(|c| c.kind() == "extern_modifier")
        .map(|n| collapse_ws(&n.text()))
        .unwrap_or_else(|| "extern".to_string());

    let mut attrs = Vec::new();
    let mut docs = Vec::new();
    _extract_attrs_and_docs(node, src, &mut attrs, &mut docs);

    let mut children = Vec::new();
    // The body is a `declaration_list` direct child of `foreign_mod_item`.
    for body in node.children().filter(|c| c.kind() == "declaration_list") {
        for item in body.children() {
            match item.kind().as_ref() {
                "function_signature_item" => {
                    children.push(_function_to_decl(&item, src, false));
                }
                "static_item" => {
                    if let Some(d) = _const_or_static_to_field(&item, src) {
                        children.push(d);
                    }
                }
                _ => {}
            }
        }
    }

    Declaration {
        kind: DeclarationKind::Namespace,
        name: abi.clone(),
        signature: abi,
        bases: Vec::new(),
        deprecated: false,
        attrs,
        docs,
        docs_inside: false,
        visibility: _visibility(node, src),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: node.range().start,
        end_byte: node.range().end,
        doc_start_byte: _doc_start(node),
        native_kind: None,
        modifiers: Vec::new(),
        children,
        calls: Vec::new(),
    }
}

fn _associated_type_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name")?;
    let mut attrs = Vec::new();
    let mut docs = Vec::new();
    _extract_attrs_and_docs(node, src, &mut attrs, &mut docs);

    let sig = collapse_ws(&String::from_utf8_lossy(
        &src[node.range().start..node.range().end],
    ))
    .trim_end_matches(';')
    .to_string();

    Some(Declaration {
        kind: DeclarationKind::Field,
        name,
        signature: sig,
        bases: Vec::new(),
        deprecated: false,
        attrs,
        docs,
        docs_inside: false,
        visibility: _visibility(node, src),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: node.range().start,
        end_byte: node.range().end,
        doc_start_byte: _doc_start(node),
        native_kind: None,
        modifiers: Vec::new(),
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _const_or_static_to_field<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name")?;
    let mut attrs = Vec::new();
    let mut docs = Vec::new();
    _extract_attrs_and_docs(node, src, &mut attrs, &mut docs);

    let sig = collapse_ws(&String::from_utf8_lossy(
        &src[node.range().start..node.range().end],
    ))
    .trim_end_matches(';')
    .to_string();

    Some(Declaration {
        kind: DeclarationKind::Field,
        name,
        signature: sig,
        bases: Vec::new(),
        deprecated: false,
        attrs,
        docs,
        docs_inside: false,
        visibility: _visibility(node, src),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: node.range().start,
        end_byte: node.range().end,
        doc_start_byte: _doc_start(node),
        native_kind: None,
        modifiers: Vec::new(),
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _field_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name")?;
    let mut attrs = Vec::new();
    let mut docs = Vec::new();
    _extract_attrs_and_docs(node, src, &mut attrs, &mut docs);

    let sig = collapse_ws(&String::from_utf8_lossy(
        &src[node.range().start..node.range().end],
    ))
    .trim_end_matches(',')
    .to_string();

    Some(Declaration {
        kind: DeclarationKind::Field,
        name,
        signature: sig,
        bases: Vec::new(),
        deprecated: false,
        attrs,
        docs,
        docs_inside: false,
        visibility: _visibility(node, src),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: node.range().start,
        end_byte: node.range().end,
        doc_start_byte: _doc_start(node),
        native_kind: None,
        modifiers: Vec::new(),
        children: Vec::new(),
        calls: Vec::new(),
    })
}

/// Tuple-struct positional field. Tree-sitter doesn't wrap these in
/// `field_declaration` nodes — `pub struct Pair(pub u8, i32)` parses as
/// alternating `visibility_modifier` + type nodes. Caller hands us the
/// type node, the running visibility, and any preceding attrs.
fn _positional_field_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    idx: usize,
    visibility: String,
    attrs: Vec<String>,
) -> Declaration {
    let type_text = collapse_ws(&String::from_utf8_lossy(
        &src[node.range().start..node.range().end],
    ));
    // Prefix the index so the outline renderer (which renders fields by
    // signature, not name) shows `0: pub u8` instead of just `pub u8`.
    let sig = if !visibility.is_empty() {
        format!("{}: {} {}", idx, visibility, type_text)
    } else {
        format!("{}: {}", idx, type_text)
    };

    Declaration {
        kind: DeclarationKind::Field,
        name: idx.to_string(),
        signature: sig,
        bases: Vec::new(),
        deprecated: false,
        attrs,
        docs: Vec::new(),
        docs_inside: false,
        visibility,
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: node.range().start,
        end_byte: node.range().end,
        doc_start_byte: node.range().start,
        native_kind: None,
        modifiers: Vec::new(),
        children: Vec::new(),
        calls: Vec::new(),
    }
}

fn _extract_attrs_and_docs<'a, D: Doc>(
    node: &Node<'a, D>,
    _src: &[u8],
    attrs: &mut Vec<String>,
    docs: &mut Vec<String>,
) {
    let mut current = node.prev();
    let mut nodes = Vec::new();
    while let Some(prev) = current {
        if prev.kind() == "line_comment"
            || prev.kind() == "block_comment"
            || prev.kind() == "attribute_item"
        {
            nodes.push(prev.clone());
            current = prev.prev();
        } else {
            break;
        }
    }
    nodes.reverse();
    for n in nodes {
        if n.kind() == "attribute_item" {
            attrs.push(collapse_ws(&n.text()));
        } else {
            let t = n.text().into_owned();
            if t.starts_with("///") || t.starts_with("/**") {
                docs.push(t);
            }
        }
    }
}

fn _doc_start<'a, D: Doc>(node: &Node<'a, D>) -> usize {
    let mut start = node.range().start;
    let mut current = node.prev();
    while let Some(prev) = current {
        if prev.kind() == "line_comment"
            || prev.kind() == "block_comment"
            || prev.kind() == "attribute_item"
        {
            start = prev.range().start;
            current = prev.prev();
        } else {
            break;
        }
    }
    start
}

fn _visibility<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> String {
    for c in node.children() {
        if c.kind() == "visibility_modifier" {
            return collapse_ws(&c.text());
        }
    }
    String::new() // Rust default is private
}
