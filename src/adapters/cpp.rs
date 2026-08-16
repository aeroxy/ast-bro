use super::base::{collapse_ws, count_parse_errors, field_text, LanguageAdapter};
use crate::core::{CallKind, CallSite, Declaration, DeclarationKind, ParseResult};
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub struct CppAdapter;

impl LanguageAdapter for CppAdapter {
    fn language_name(&self) -> &'static str {
        "cpp"
    }

    fn parse<'a, D: Doc>(&self, path: &Path, source: &[u8], root: Node<'a, D>) -> ParseResult {
        let mut decls = Vec::new();
        _walk_namespace(&root, source, &mut decls);
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

fn _walk_namespace<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<Declaration>) {
    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        let kind = child.kind();
        if kind == "namespace_definition" {
            if let Some(decl) = _namespace_to_decl(&child, src) {
                out.push(decl);
            }
        } else if kind == "class_specifier" || kind == "struct_specifier" {
            if let Some(decl) = _class_to_decl(&child, src) {
                out.push(decl);
            }
        } else if kind == "function_definition" {
            if let Some(decl) = _function_to_decl(&child, src, false) {
                out.push(decl);
            }
        } else if kind == "template_declaration" {
            if let Some(decl) = _template_to_decl(&child, src) {
                out.push(decl);
            }
        } else if kind == "enum_specifier" {
            if let Some(decl) = _enum_to_decl(&child, src) {
                out.push(decl);
            }
        }
    }
}

fn _namespace_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let body = node.field("body")?;
    let mut children = Vec::new();
    for child in body.children() {
        if !child.is_named() {
            continue;
        }
        let kind = child.kind();
        if kind == "class_specifier" || kind == "struct_specifier" {
            if let Some(decl) = _class_to_decl(&child, src) {
                children.push(decl);
            }
        } else if kind == "function_definition" {
            if let Some(decl) = _function_to_decl(&child, src, false) {
                children.push(decl);
            }
        } else if kind == "template_declaration" {
            if let Some(decl) = _template_to_decl(&child, src) {
                children.push(decl);
            }
        } else if kind == "enum_specifier" {
            if let Some(decl) = _enum_to_decl(&child, src) {
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

fn _class_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let kind_str = if node.kind() == "struct_specifier" {
        "struct"
    } else {
        "class"
    };

    let bases = _class_bases(node);
    let body = node.field("body");
    let mut children = Vec::new();

    if let Some(b) = body {
        for child in b.children() {
            if !child.is_named() {
                continue;
            }
            let kind = child.kind();
            if kind == "function_definition" {
                if let Some(decl) = _function_to_decl(&child, src, true) {
                    children.push(decl);
                }
            } else if kind == "field_declaration" {
                // tree-sitter-cpp uses `field_declaration` for both data
                // members and method declarations. A function-typed declarator
                // means it's a method.
                let is_method = child
                    .field("declarator")
                    .is_some_and(|d| d.kind() == "function_declarator");
                if is_method {
                    if let Some(decl) = _method_decl_to_decl(&child, src) {
                        children.push(decl);
                    }
                } else if let Some(decl) = _field_to_decl(&child, src) {
                    children.push(decl);
                }
            } else if kind == "declaration" {
                // Constructors and destructors have no return type and parse
                // as `declaration` rather than `field_declaration`.
                if child
                    .field("declarator")
                    .is_some_and(|d| d.kind() == "function_declarator")
                {
                    if let Some(decl) = _ctor_decl_to_decl(&child, src) {
                        children.push(decl);
                    }
                }
            }
        }
    }

    let mut sig = format!("{} {}", kind_str, name);
    if !bases.is_empty() {
        sig.push_str(" : ");
        sig.push_str(&bases.join(", "));
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

/// Build a constructor/destructor `Declaration` from a `declaration` whose
/// declarator is a `function_declarator` (no return type).
fn _ctor_decl_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<Declaration> {
    let declarator = node.field("declarator")?;
    let inner = declarator.field("declarator")?;
    let raw_name = collapse_ws(&inner.text()).trim().to_string();

    let mut params = Vec::new();
    if let Some(pl) = declarator.field("parameters") {
        for child in pl.children() {
            if child.is_named() && child.kind() == "parameter_declaration" {
                let text = collapse_ws(&child.text());
                if !text.is_empty() {
                    params.push(text);
                }
            }
        }
    }

    let kind = if raw_name.starts_with('~') {
        DeclarationKind::Destructor
    } else {
        DeclarationKind::Constructor
    };

    let sig = format!("{}({})", raw_name, params.join(", "));
    let range = node.range();
    Some(Declaration {
        kind,
        name: raw_name,
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
        native_kind: Some("ctor".to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

/// Build a method `Declaration` from a `field_declaration` whose declarator
/// is a `function_declarator` (i.e. method declaration without a body).
fn _method_decl_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<Declaration> {
    let declarator = node.field("declarator")?;
    // The function_declarator's own `declarator` field carries the method name.
    let inner = declarator.field("declarator")?;
    let raw_name = collapse_ws(&inner.text()).trim().to_string();
    let return_type = node
        .field("type")
        .map(|t| collapse_ws(&t.text()).trim().to_string());

    let mut params = Vec::new();
    if let Some(pl) = declarator.field("parameters") {
        for child in pl.children() {
            if child.is_named() && child.kind() == "parameter_declaration" {
                let text = collapse_ws(&child.text());
                if !text.is_empty() {
                    params.push(text);
                }
            }
        }
    }

    let kind = if raw_name.starts_with('~') {
        DeclarationKind::Destructor
    } else if raw_name.contains("operator") {
        DeclarationKind::Operator
    } else {
        DeclarationKind::Method
    };

    let mut sig = String::new();
    if let Some(rt) = return_type {
        if !rt.is_empty() {
            sig.push_str(&rt);
            sig.push(' ');
        }
    }
    sig.push_str(&raw_name);
    sig.push('(');
    sig.push_str(&params.join(", "));
    sig.push(')');

    let range = node.range();
    Some(Declaration {
        kind,
        name: raw_name,
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
        native_kind: Some("method".to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _function_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    inside_class: bool,
) -> Option<Declaration> {
    let name = _function_definition_name(node)
        .or_else(|| field_text(node, "name"))
        .unwrap_or_else(|| "?".to_string());

    // Bare `name` is used for symbol lookup (callers/callees suffix matching),
    // so for `void Greeter::greet()` it's just `greet`. The signature wants
    // the full `Greeter::greet` so the rendered output preserves scope.
    let sig_name = _function_definition_qualified_name(node).unwrap_or_else(|| name.clone());

    let return_type = node
        .field("type")
        .map(|t| collapse_ws(&t.text()).trim().to_string());

    let params = _function_params(node, src);

    let kind = if inside_class {
        if name.contains("operator") {
            DeclarationKind::Operator
        } else if name.starts_with('~') {
            DeclarationKind::Destructor
        } else {
            DeclarationKind::Method
        }
    } else {
        DeclarationKind::Function
    };

    let mut sig = String::new();
    if let Some(rt) = return_type {
        sig.push_str(&rt);
        sig.push(' ');
    }
    sig.push_str(&sig_name);
    sig.push('(');
    sig.push_str(&params.join(", "));
    sig.push(')');

    let calls = _extract_calls(node, src);
    let range = node.range();
    Some(Declaration {
        kind,
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
        native_kind: Some(if inside_class {
            "method"
        } else {
            "function"
        }
        .to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls,
    })
}

fn _template_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        let kind = child.kind();
        if kind == "class_specifier" || kind == "struct_specifier" {
            return _class_to_decl(&child, src);
        } else if kind == "function_definition" {
            return _function_to_decl(&child, src, false);
        }
    }
    None
}

fn _enum_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let sig = format!("enum {}", name);
    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Enum,
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
        native_kind: Some("enum".to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _field_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<Declaration> {
    let decl = node.field("declarator")?;
    let name = collapse_ws(&decl.text()).trim().to_string();
    let type_str = node
        .field("type")
        .map(|t| collapse_ws(&t.text()).trim().to_string());

    let sig = if let Some(t) = type_str {
        format!("{} {}", t, name)
    } else {
        name.clone()
    };

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
        native_kind: Some("field".to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

/// Base classes of a `class` or `struct`.
///
/// The base clause is an ordinary child of the specifier, not a field on
/// it — asking for `node.field("base_class_clause")` returned nothing, so
/// C++ recorded no supertypes at all and `implements` had no C++ edges to
/// walk. Inside the clause the type names are what is left once the parts
/// that are not types are dropped: `public`, `private` and `protected` are
/// `access_specifier` nodes, and `virtual`, the commas and the leading
/// colon are anonymous. The rule names those rather than the type kinds it
/// expects, because a kind this list forgot — `decltype(expr)`, or whatever
/// a later grammar splits out of `qualified_identifier` — is dropped in
/// silence, which reads as "this class has no base".
fn _class_bases<'a, D: Doc>(node: &Node<'a, D>) -> Vec<String> {
    let mut bases = Vec::new();
    for bc in node.children().filter(|c| c.kind() == "base_class_clause") {
        for child in bc.children() {
            let skip = matches!(child.kind().as_ref(), "access_specifier" | "comment");
            if !child.is_named() || skip {
                continue;
            }
            let text = collapse_ws(&child.text());
            if !text.is_empty() {
                bases.push(text);
            }
        }
    }
    bases
}

fn _function_params<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    let mut params = Vec::new();
    let param_list = node.field("parameters");
    if let Some(pl) = param_list {
        for child in pl.children() {
            if child.is_named() && child.kind() == "parameter_declaration" {
                let text = collapse_ws(&child.text());
                if !text.is_empty() {
                    params.push(text);
                }
            }
        }
    }
    params
}

/// Extract just the bare callable name from a `function_definition` node by
/// drilling through its `function_declarator` to the inner declarator.
/// `field_text(node, "declarator")` returns the full `greet()` text instead
/// of `greet`, which breaks suffix matching for `callers` / `callees`.
fn _function_definition_name<'a, D: Doc>(node: &Node<'a, D>) -> Option<String> {
    let declarator = node.field("declarator")?;
    _drill_function_declarator_name(&declarator)
}

fn _drill_function_declarator_name<'a, D: Doc>(node: &Node<'a, D>) -> Option<String> {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "function_declarator" => {
            let inner = node.field("declarator")?;
            _drill_function_declarator_name(&inner)
        }
        "pointer_declarator" | "reference_declarator" | "parenthesized_declarator" => {
            let inner = node.field("declarator").or_else(|| {
                node.children().find(|c| c.is_named())
            })?;
            _drill_function_declarator_name(&inner)
        }
        "identifier" | "field_identifier" | "operator_name" | "destructor_name" => {
            Some(collapse_ws(&node.text()).trim().to_string())
        }
        "qualified_identifier" | "scoped_identifier" => {
            // For out-of-line method definitions: `Foo::bar` → just `bar`.
            let text = collapse_ws(&node.text());
            Some(text.rsplit("::").next().unwrap_or(&text).trim().to_string())
        }
        _ => Some(collapse_ws(&node.text()).trim().to_string()),
    }
}

/// Sibling of `_function_definition_name` that preserves scope qualifiers
/// for the rendered signature. `void Greeter::greet()` → `Greeter::greet`,
/// keeping the `Foo::` that the bare-name extractor strips for lookup.
fn _function_definition_qualified_name<'a, D: Doc>(node: &Node<'a, D>) -> Option<String> {
    let declarator = node.field("declarator")?;
    _drill_function_declarator_qname(&declarator)
}

fn _drill_function_declarator_qname<'a, D: Doc>(node: &Node<'a, D>) -> Option<String> {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "function_declarator" => {
            let inner = node.field("declarator")?;
            _drill_function_declarator_qname(&inner)
        }
        "pointer_declarator" | "reference_declarator" | "parenthesized_declarator" => {
            let inner = node.field("declarator").or_else(|| {
                node.children().find(|c| c.is_named())
            })?;
            _drill_function_declarator_qname(&inner)
        }
        // Explicit terminals — preserve the full scope (`Foo::bar`) for
        // signature rendering. The catch-all below would also return the
        // full text today, but pinning these makes the intent explicit and
        // protects against tree-sitter-cpp grammar drift introducing a new
        // wrapper node that the catch-all would silently truncate.
        "qualified_identifier"
        | "scoped_identifier"
        | "identifier"
        | "field_identifier"
        | "operator_name"
        | "destructor_name" => Some(collapse_ws(&node.text()).trim().to_string()),
        _ => Some(collapse_ws(&node.text()).trim().to_string()),
    }
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
            | "lambda_expression"
            | "class_specifier"
            | "struct_specifier"
            | "namespace_definition"
    ) {
        return;
    }

    if kind == "call_expression" {
        if let Some(cs) = _call_site_from_call_cpp(node, src) {
            out.push(cs);
        }
    } else if kind == "new_expression" {
        if let Some(cs) = _call_site_from_new_expression(node, src) {
            out.push(cs);
        }
    }

    for child in node.children() {
        _walk_calls_in_body(&child, src, out);
    }
}

fn _call_site_from_call_cpp<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<CallSite> {
    let func = node.field("function")?;
    let (name, receiver) = _split_callee_cpp(&func, src)?;
    let line = node.start_pos().line() as u32 + 1;
    Some(CallSite {
        name,
        receiver,
        line,
        kind: CallKind::Call,
    })
}

fn _call_site_from_new_expression<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<CallSite> {
    let type_node = node.field("type").or_else(|| {
        node.children()
            .find(|c| c.is_named() && c.kind() != "new")
    })?;
    let raw = String::from_utf8_lossy(&src[type_node.range()]).to_string();
    let name = _last_type_segment_cpp(&raw);
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

fn _split_callee_cpp<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<(String, Option<String>)> {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "identifier" | "field_identifier" | "destructor_name" => {
            let text = String::from_utf8_lossy(&src[node.range()]).to_string();
            Some((text, None))
        }
        "field_expression" => {
            let field = node.field("field")?;
            let arg = node.field("argument");
            let name = String::from_utf8_lossy(&src[field.range()]).to_string();
            let receiver = arg
                .map(|a| collapse_ws(&String::from_utf8_lossy(&src[a.range()])));
            Some((name, receiver))
        }
        "qualified_identifier" | "scoped_identifier" => {
            let raw = String::from_utf8_lossy(&src[node.range()]).to_string();
            let collapsed = collapse_ws(&raw);
            let name = collapsed.rsplit("::").next().unwrap_or(&collapsed).to_string();
            let receiver = if collapsed.contains("::") {
                let cut = collapsed.rfind("::").unwrap();
                Some(collapsed[..cut].to_string())
            } else {
                None
            };
            Some((name, receiver))
        }
        "template_function" => {
            let name_node = node.field("name").or_else(|| {
                node.children().find(|c| c.is_named())
            })?;
            _split_callee_cpp(&name_node, src)
        }
        _ => {
            let raw = String::from_utf8_lossy(&src[node.range()]).to_string();
            let collapsed = collapse_ws(&raw);
            if collapsed.is_empty() {
                return None;
            }
            let name = collapsed
                .rsplit("::")
                .next()
                .unwrap_or(&collapsed)
                .rsplit('.')
                .next()
                .unwrap_or(&collapsed)
                .split('<')
                .next()
                .unwrap_or(&collapsed)
                .trim()
                .to_string();
            if name.is_empty() {
                return None;
            }
            Some((name, None))
        }
    }
}

fn _last_type_segment_cpp(text: &str) -> String {
    let head = text.split('<').next().unwrap_or(text).trim();
    head.rsplit("::")
        .next()
        .unwrap_or(head)
        .rsplit('.')
        .next()
        .unwrap_or(head)
        .trim()
        .to_string()
}
