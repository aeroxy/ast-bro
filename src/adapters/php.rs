use super::base::{collapse_ws, count_parse_errors, field_text, LanguageAdapter};
use crate::core::{CallKind, CallSite, Declaration, DeclarationKind, ParseResult};
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub struct PhpAdapter;

impl LanguageAdapter for PhpAdapter {
    fn language_name(&self) -> &'static str {
        "php"
    }

    fn parse<'a, D: Doc>(&self, path: &Path, source: &[u8], root: Node<'a, D>) -> ParseResult {
        let mut decls = Vec::new();
        _walk_program(&root, source, &mut decls);
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

fn _walk_program<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<Declaration>) {
    // `namespace X;` has no body, so everything after it belongs to it.
    // Leaving those declarations as siblings of the namespace left them with
    // no package at all, which is what a namespace-qualified supertype has
    // to be compared against — `implements \Ns\Root` had nothing to match.
    // The braced form `namespace X { … }` carries its own body and is
    // handled by `_namespace_to_decl`.
    let mut open: Option<Declaration> = None;
    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        let statement_namespace =
            child.kind() == "namespace_definition" && child.field("body").is_none();
        let Some(decl) = _node_to_decl(&child, src, false) else {
            continue;
        };
        if statement_namespace {
            if let Some(previous) = open.take() {
                out.push(previous);
            }
            open = Some(decl);
        } else if let Some(ns) = &mut open {
            ns.end_line = decl.end_line;
            ns.end_byte = decl.end_byte;
            ns.children.push(decl);
        } else {
            out.push(decl);
        }
    }
    if let Some(ns) = open {
        out.push(ns);
    }
}

fn _node_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    inside_class: bool,
) -> Option<Declaration> {
    let kind = node.kind();

    if kind == "class_declaration" || kind == "interface_declaration" || kind == "trait_declaration" {
        return _class_to_decl(node, src);
    } else if kind == "function_definition" {
        return _function_to_decl(node, src, inside_class);
    } else if kind == "method_declaration" {
        return _method_to_decl(node, src);
    } else if kind == "namespace_definition" {
        return _namespace_to_decl(node, src);
    } else if kind == "const_declaration" || kind == "class_const_declaration" {
        return _const_to_decl(node, src);
    } else if kind == "property_declaration" {
        return _property_to_decl(node, src);
    }

    None
}

fn _class_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());

    let kind = node.kind();
    let kind_str = if kind == "interface_declaration" {
        "interface"
    } else if kind == "trait_declaration" {
        "trait"
    } else {
        "class"
    };

    let extends = _class_extends(node);
    let implements = _class_implements(node);

    let body = node.field("body");
    let mut children = Vec::new();

    if let Some(b) = body {
        for child in b.children() {
            if !child.is_named() {
                continue;
            }
            if let Some(decl) = _node_to_decl(&child, src, true) {
                children.push(decl);
            }
        }
    }

    let mut sig = format!("{} {}", kind_str, name);
    let mut bases = Vec::new();

    if !extends.is_empty() {
        sig.push_str(" extends ");
        sig.push_str(&extends.join(", "));
        bases.extend(extends);
    }

    if !implements.is_empty() {
        sig.push_str(" implements ");
        sig.push_str(&implements.join(", "));
        bases.extend(implements);
    }

    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Class,
        name: name.clone(),
        signature: sig,
        bases,
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
        visibility: String::new(),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: range.start,
        native_kind: Some(kind_str.to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children,
        calls: Vec::new(),
    })
}

fn _function_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    _inside_class: bool,
) -> Option<Declaration> {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let params = _function_params(node, src);

    let return_type = node
        .field("return_type")
        .map(|t| collapse_ws(&t.text()).trim().to_string());

    let mut sig = format!("function {}({})", name, params.join(", "));
    if let Some(rt) = return_type {
        sig.push_str(": ");
        sig.push_str(&rt);
    }

    let calls = _extract_calls(node, src);
    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Function,
        name: name.clone(),
        signature: sig,
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
        native_kind: Some("function".to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls,
    })
}

fn _method_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let params = _function_params(node, src);

    let return_type = node
        .field("return_type")
        .map(|t| collapse_ws(&t.text()).trim().to_string());

    let (visibility, modifiers) = _php_modifiers(node);

    let mut sig = String::new();
    if !visibility.is_empty() {
        sig.push_str(&visibility);
        sig.push(' ');
    }
    for m in &modifiers {
        sig.push_str(m);
        sig.push(' ');
    }
    sig.push_str(&format!("function {}({})", name, params.join(", ")));
    if let Some(rt) = return_type {
        sig.push_str(": ");
        sig.push_str(&rt);
    }

    let calls = _extract_calls(node, src);
    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Method,
        name: name.clone(),
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
        visibility,
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: range.start,
        native_kind: Some("method".to_string()),
        modifiers,
        deprecated: false,
        children: Vec::new(),
        calls,
    })
}

/// Pull `visibility_modifier` (public/private/protected) and other static/abstract/final
/// markers off a `method_declaration` or `property_declaration`. tree-sitter-php emits
/// these as direct children of the declaration, not as named fields.
fn _php_modifiers<'a, D: Doc>(node: &Node<'a, D>) -> (String, Vec<String>) {
    let mut visibility = String::new();
    let mut modifiers = Vec::new();
    for child in node.children() {
        let kind = child.kind();
        let kind_str: &str = kind.as_ref();
        if kind_str == "visibility_modifier" {
            visibility = collapse_ws(&child.text()).trim().to_lowercase();
        } else if matches!(
            kind_str,
            "static_modifier" | "abstract_modifier" | "final_modifier" | "readonly_modifier"
        ) {
            // tree-sitter-php names these `<keyword>_modifier`. Strip the suffix.
            modifiers.push(kind_str.trim_end_matches("_modifier").to_string());
        }
    }
    (visibility, modifiers)
}

fn _namespace_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let body = node.field("body");
    let mut children = Vec::new();

    if let Some(b) = body {
        for child in b.children() {
            if !child.is_named() {
                continue;
            }
            if let Some(decl) = _node_to_decl(&child, src, false) {
                children.push(decl);
            }
        }
    }

    let sig = format!("namespace {}", name);
    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Namespace,
        name: name.clone(),
        signature: sig,
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
        native_kind: Some("namespace".to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children,
        calls: Vec::new(),
    })
}

fn _const_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<Declaration> {
    let name = node
        .field("left")
        .or_else(|| node.field("name"))
        .map(|n| collapse_ws(&n.text()).trim().to_string())
        .unwrap_or_else(|| "?".to_string());

    let sig = format!("const {}", name);
    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Field,
        name,
        signature: sig,
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
        native_kind: Some("const".to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _property_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<Declaration> {
    let mut names = Vec::new();
    for child in node.children() {
        if child.kind() == "property_element" {
            if let Some(name) = child.field("name") {
                names.push(collapse_ws(&name.text()).trim().to_string());
            }
        }
    }

    if names.is_empty() {
        return None;
    }

    let (visibility, modifiers) = _php_modifiers(node);

    // PHP property names from tree-sitter already include the `$` prefix.
    let mut sig = String::new();
    if !visibility.is_empty() {
        sig.push_str(&visibility);
        sig.push(' ');
    }
    for m in &modifiers {
        sig.push_str(m);
        sig.push(' ');
    }
    sig.push_str(&format!("{} ...", names[0]));
    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Field,
        name: names.join(", "),
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
        visibility,
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: range.start,
        native_kind: Some("property".to_string()),
        modifiers,
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

/// Supertypes named by `extends`, which is one class or several
/// interfaces.
fn _class_extends<'a, D: Doc>(node: &Node<'a, D>) -> Vec<String> {
    _clause_type_names(node, "base_clause")
}

/// Supertypes named by `implements`.
fn _class_implements<'a, D: Doc>(node: &Node<'a, D>) -> Vec<String> {
    _clause_type_names(node, "class_interface_clause")
}

/// The type names inside a supertype clause.
///
/// The clause is an ordinary child of the declaration, not a field on it —
/// asking for `node.field("extends")` returned nothing, so PHP recorded no
/// supertypes at all and `implements` had no PHP edges to walk. What is
/// left after the parts that are not types is the answer: the `extends` /
/// `implements` keywords and the separating commas are anonymous, and a
/// comment is the one named child that names nothing. Listing what to drop
/// rather than the name kinds to keep is deliberate — a kind such a list
/// forgets is dropped in silence, which reads as "this class has no base".
fn _clause_type_names<'a, D: Doc>(node: &Node<'a, D>, clause: &str) -> Vec<String> {
    let mut out = Vec::new();
    for c in node.children().filter(|c| c.kind() == clause) {
        for name in c.children() {
            if !name.is_named() || name.kind() == "comment" {
                continue;
            }
            let text = collapse_ws(&name.text()).trim().to_string();
            if !text.is_empty() {
                out.push(text);
            }
        }
    }
    out
}

fn _function_params<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    let mut params = Vec::new();
    let param_list = node.field("parameters");
    if let Some(pl) = param_list {
        for child in pl.children() {
            if child.is_named() {
                let text = collapse_ws(&child.text()).trim().to_string();
                if !text.is_empty() {
                    params.push(text);
                }
            }
        }
    }
    params
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
            | "method_declaration"
            | "anonymous_function"
            | "arrow_function"
            | "class_declaration"
            | "interface_declaration"
            | "trait_declaration"
            | "enum_declaration"
    ) {
        return;
    }

    match kind {
        "function_call_expression" => {
            if let Some(cs) = _call_site_from_function_call_php(node, src) {
                out.push(cs);
            }
        }
        "member_call_expression" | "nullsafe_member_call_expression" => {
            if let Some(cs) = _call_site_from_member_call_php(node, src) {
                out.push(cs);
            }
        }
        "scoped_call_expression" => {
            if let Some(cs) = _call_site_from_scoped_call_php(node, src) {
                out.push(cs);
            }
        }
        "object_creation_expression" => {
            if let Some(cs) = _call_site_from_object_creation_php(node, src) {
                out.push(cs);
            }
        }
        _ => {}
    }

    for child in node.children() {
        _walk_calls_in_body(&child, src, out);
    }
}

fn _call_site_from_function_call_php<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<CallSite> {
    let func = node.field("function")?;
    let (name, receiver) = _split_callee_php(&func, src)?;
    let line = node.start_pos().line() as u32 + 1;
    Some(CallSite {
        name,
        receiver,
        line,
        kind: CallKind::Call,
    })
}

fn _call_site_from_member_call_php<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<CallSite> {
    let name_node = node.field("name")?;
    let object = node.field("object");
    let name = _bare_name_text_php(&name_node, src);
    if name.is_empty() {
        return None;
    }
    let receiver = object.map(|o| collapse_ws(&String::from_utf8_lossy(&src[o.range()])));
    let line = node.start_pos().line() as u32 + 1;
    Some(CallSite {
        name,
        receiver,
        line,
        kind: CallKind::Call,
    })
}

fn _call_site_from_scoped_call_php<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<CallSite> {
    let name_node = node.field("name")?;
    let scope = node.field("scope");
    let name = _bare_name_text_php(&name_node, src);
    if name.is_empty() {
        return None;
    }
    // Strip a leading `\` from the namespaced scope so `\Foo\Greeter` and
    // `Foo\Greeter` normalise to the same receiver — otherwise the resolver
    // sees them as two distinct receiver types. Also drop the late-binding
    // keywords (`self`, `static`, `parent`) — they aren't class names, so
    // the resolver should treat the call as having no receiver and let
    // pass B match the bare method name in the global symbol table.
    let receiver = scope.and_then(|s| {
        let text = collapse_ws(&String::from_utf8_lossy(&src[s.range()]))
            .trim_start_matches('\\')
            .to_string();
        // PHP keywords are case-insensitive: `SELF::` / `Static::` are the
        // same late-binding scopes as their lowercase forms.
        if matches!(
            text.to_ascii_lowercase().as_str(),
            "self" | "static" | "parent"
        ) {
            None
        } else {
            Some(text)
        }
    });
    let line = node.start_pos().line() as u32 + 1;
    Some(CallSite {
        name,
        receiver,
        line,
        kind: CallKind::Call,
    })
}

fn _call_site_from_object_creation_php<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<CallSite> {
    // `object_creation_expression` has no fields; the class reference is the
    // first named child that isn't `arguments` or `anonymous_class`.
    let class_ref = node.children().find(|c| {
        if !c.is_named() {
            return false;
        }
        let k = c.kind();
        let k: &str = k.as_ref();
        !matches!(k, "arguments" | "anonymous_class")
    })?;
    // Unwrap `new (Class)()` — the class ref appears wrapped in a
    // parenthesized_expression. Drill once to find the inner reference.
    let class_ref = if class_ref.kind().as_ref() == "parenthesized_expression" {
        class_ref.clone().children().find(|c| c.is_named()).unwrap_or(class_ref)
    } else {
        class_ref
    };
    // Dynamic class instantiation (`new $cls()`, `new ${...}`) names a
    // runtime value, not a callable — skip rather than emit a `$cls` edge
    // that no resolver pass can ever match.
    let kind_str = class_ref.kind();
    let kind_str: &str = kind_str.as_ref();
    if matches!(kind_str, "variable_name" | "dynamic_variable_name") {
        return None;
    }
    let raw = String::from_utf8_lossy(&src[class_ref.range()]).to_string();
    let name = _last_php_name_segment(&raw);
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

fn _split_callee_php<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<(String, Option<String>)> {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "name" => {
            let text = String::from_utf8_lossy(&src[node.range()]).to_string();
            Some((text, None))
        }
        "variable_name" | "dynamic_variable_name" => {
            // `$func()` calls a value, not a named function — no static
            // resolution is possible. Skip rather than emit a `$func` edge
            // that no resolver pass can ever match.
            None
        }
        "qualified_name" | "relative_name" => {
            // `\Foo\bar()` is a free-function call with a namespace prefix,
            // not a method call on `Foo`. Drop the namespace and emit the
            // bare function name with `receiver: None` so pass A/B in
            // resolve.rs can match it against the global symbol table —
            // the resolver gates pass B promotion on the absence of a
            // receiver, and PHP namespace-prefixed calls are still free
            // functions semantically.
            let raw = String::from_utf8_lossy(&src[node.range()]).to_string();
            let collapsed = collapse_ws(&raw);
            let name = collapsed
                .rsplit('\\')
                .next()
                .unwrap_or(&collapsed)
                .trim()
                .to_string();
            if name.is_empty() {
                return None;
            }
            Some((name, None))
        }
        "parenthesized_expression" => {
            let inner = node.children().find(|c| c.is_named())?;
            _split_callee_php(&inner, src)
        }
        _ => {
            let raw = String::from_utf8_lossy(&src[node.range()]).to_string();
            let collapsed = collapse_ws(&raw);
            if collapsed.is_empty() {
                return None;
            }
            Some((collapsed, None))
        }
    }
}

fn _bare_name_text_php<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let raw = String::from_utf8_lossy(&src[node.range()]).to_string();
    collapse_ws(&raw).trim().to_string()
}

fn _last_php_name_segment(text: &str) -> String {
    let collapsed = collapse_ws(text);
    let trimmed = collapsed.trim();
    trimmed
        .rsplit('\\')
        .next()
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}
