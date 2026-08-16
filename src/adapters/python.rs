use super::base::{collapse_ws, count_parse_errors, field_text, LanguageAdapter};
use crate::core::{CallKind, CallSite, Declaration, DeclarationKind, ImportBinding, ParseResult};
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub struct PythonAdapter;

impl LanguageAdapter for PythonAdapter {
    fn language_name(&self) -> &'static str {
        "python"
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
        if let Some(decl) = _node_to_decl(&child, src, false) {
            out.push(decl);
        }
    }
}

fn _walk_class_body<'a, D: Doc>(block: &Node<'a, D>, src: &[u8]) -> Vec<Declaration> {
    let mut children = Vec::new();
    for c in block.children() {
        if !c.is_named() {
            continue;
        }
        if let Some(decl) = _node_to_decl(&c, src, true) {
            children.push(decl);
        }
    }
    children
}

fn _node_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    inside_class: bool,
) -> Option<Declaration> {
    let kind = node.kind();

    if kind == "decorated_definition" {
        let mut decorators = Vec::new();
        for c in node.children() {
            if c.kind() == "decorator" {
                decorators.push(collapse_ws(&c.text()));
            }
        }
        let definition = node.field("definition")?;
        let mut decl = _node_to_decl(&definition, src, inside_class)?;

        let mut new_attrs = decorators.clone();
        new_attrs.extend(decl.attrs);
        decl.attrs = new_attrs;

        decl.start_line = node.start_pos().line() + 1;
        decl.start_byte = node.range().start;
        let ds_byte = if decl.doc_start_byte > 0 {
            std::cmp::min(decl.doc_start_byte, node.range().start)
        } else {
            node.range().start
        };
        decl.doc_start_byte = ds_byte;

        if inside_class
            && decl.kind == DeclarationKind::Method
            && decorators.iter().any(|d| {
                d == "@property" || d.starts_with("@property ") || d.starts_with("@property\n")
            })
        {
            decl.kind = DeclarationKind::Property;
        }
        return Some(decl);
    }

    if kind == "class_definition" {
        return Some(_class_to_decl(node, src));
    }

    if kind == "function_definition" {
        return Some(_function_to_decl(node, src, inside_class));
    }

    if kind == "expression_statement" {
        for inner in node.children() {
            if inner.kind() == "assignment" {
                return _assignment_to_decl(&inner, src);
            }
        }
        return None;
    }

    if kind == "assignment" {
        return _assignment_to_decl(node, src);
    }

    None
}

fn _class_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let bases = _class_bases(node, src);
    let body = node.field("body");

    let docs = body
        .as_ref()
        .map(|b| _docstring(b, src))
        .unwrap_or_default();
    let children = body
        .as_ref()
        .map(|b| _walk_class_body(b, src))
        .unwrap_or_default();

    let mut sig = format!("class {}", name);
    if !bases.is_empty() {
        sig.push('(');
        sig.push_str(&bases.join(", "));
        sig.push(')');
    }

    let range = node.range();
    Declaration {
        kind: DeclarationKind::Class,
        name: name.clone(),
        signature: sig,
        bases,
        attrs: Vec::new(),
        docs,
        docs_inside: true,
        group: None,
        visibility: _visibility_for_name(&name),
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

fn _function_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    inside_class: bool,
) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let body = node.field("body");

    let docs = body
        .as_ref()
        .map(|b| _docstring(b, src))
        .unwrap_or_default();
    let sig = _function_signature(node, src);

    let kind = if inside_class && name == "__init__" {
        DeclarationKind::Constructor
    } else if inside_class {
        DeclarationKind::Method
    } else {
        DeclarationKind::Function
    };

    let range = node.range();
    let mut calls = Vec::new();
    if let Some(b) = body.as_ref() {
        _walk_calls_in_body(b, src, &mut calls);
    }
    Declaration {
        kind,
        name: name.clone(),
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs,
        docs_inside: true,
        group: None,
        visibility: _visibility_for_name(&name),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: range.start,
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls,
    }
}

fn _assignment_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<Declaration> {
    let left = node.field("left")?;
    if left.kind() != "identifier" {
        return None;
    }
    let name = left.text().into_owned();

    let type_str = node
        .field("type")
        .map(|t| t.text().trim_start_matches(':').trim().to_string());

    let sig = if let Some(t) = type_str {
        format!("{}: {}", name, t)
    } else {
        name.clone()
    };

    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Field,
        name: name.clone(),
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
        group: None,
        visibility: _visibility_for_name(&name),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: 0,
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _function_signature<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let body = node.field("body");
    let end_byte = body.map(|b| b.range().start).unwrap_or(node.range().end);
    let start_byte = node.range().start;

    let text = String::from_utf8_lossy(&src[start_byte..end_byte]).to_string();
    let text = collapse_ws(&text);
    text.trim_end_matches(&[' ', ':'][..]).to_string()
}

fn _class_bases<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    let sup = node.field("superclasses");
    let mut out = Vec::new();
    if let Some(s) = sup {
        for c in s.children() {
            if !c.is_named() {
                continue;
            }
            if c.kind() == "keyword_argument" {
                continue;
            }
            let t = collapse_ws(&c.text());
            if !t.is_empty() {
                out.push(t);
            }
        }
    }
    out
}

fn _docstring<'a, D: Doc>(block: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    for c in block.children() {
        if !c.is_named() {
            continue;
        }
        if c.kind() == "expression_statement" {
            let mut inner = c.children().filter(|child| child.is_named());
            if let Some(i) = inner.next() {
                if i.kind() == "string" || i.kind() == "concatenated_string" {
                    let text = i.text().into_owned();
                    return text.lines().map(|s| s.to_string()).collect();
                }
            }
        }
        break;
    }
    Vec::new()
}

fn _visibility_for_name(name: &str) -> String {
    if name.starts_with("__") && name.ends_with("__") {
        String::new()
    } else if name.starts_with('_') {
        "private".to_string()
    } else {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Call-site extraction
// ---------------------------------------------------------------------------

fn _walk_calls_in_body<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<CallSite>) {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    if matches!(
        kind,
        "function_definition" | "class_definition" | "lambda" | "decorated_definition"
    ) {
        return;
    }

    if kind == "call" {
        if let Some(cs) = _call_site_from_call(node, src) {
            out.push(cs);
        }
    }

    for child in node.children() {
        _walk_calls_in_body(&child, src, out);
    }
}

fn _call_site_from_call<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<CallSite> {
    let func = node.field("function")?;
    let (name, receiver) = _extract_callee_name(&func, src)?;
    let line = node.start_pos().line() as u32 + 1;
    Some(CallSite {
        name,
        receiver,
        line,
        kind: CallKind::Call,
    })
}

fn _extract_callee_name<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<(String, Option<String>)> {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "identifier" => Some((String::from_utf8_lossy(&src[node.range()]).to_string(), None)),
        "attribute" => {
            // obj.method or pkg.mod.fn — last attribute is the name, prefix is receiver.
            let object = node.field("object");
            let attr = node.field("attribute")?;
            let name = String::from_utf8_lossy(&src[attr.range()]).to_string();
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
//   import foo
//   import foo as bar
//   import foo.bar
//   from foo import bar
//   from foo import bar as baz
//   from foo import a, b
//   from . import x
// ---------------------------------------------------------------------------

fn _walk_imports<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<ImportBinding>) {
    for child in node.children() {
        let kind = child.kind();
        let kind: &str = kind.as_ref();
        let line = child.start_pos().line() as u32 + 1;
        match kind {
            "import_statement" => _handle_import_statement(&child, src, line, out),
            "import_from_statement" => _handle_import_from(&child, src, line, out),
            _ => {}
        }
    }
}

fn _handle_import_statement<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    line: u32,
    out: &mut Vec<ImportBinding>,
) {
    for child in node.children() {
        let k = child.kind();
        let k: &str = k.as_ref();
        match k {
            "dotted_name" => {
                let module = String::from_utf8_lossy(&src[child.range()]).to_string();
                let local = module.split('.').next_back().unwrap_or(&module).to_string();
                out.push(ImportBinding { local, module, line });
            }
            "aliased_import" => {
                let name = child.field("name");
                let alias = child.field("alias");
                if let (Some(n), Some(a)) = (name, alias) {
                    let module = String::from_utf8_lossy(&src[n.range()]).to_string();
                    let local = String::from_utf8_lossy(&src[a.range()]).to_string();
                    out.push(ImportBinding { local, module, line });
                }
            }
            _ => {}
        }
    }
}

fn _handle_import_from<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    line: u32,
    out: &mut Vec<ImportBinding>,
) {
    let module_node = node.field("module_name");
    let module_prefix = module_node
        .as_ref()
        .map(|m| String::from_utf8_lossy(&src[m.range()]).to_string())
        .unwrap_or_default();
    let module_range = module_node.as_ref().map(|m| m.range());

    // The `name` field is repeated (multiple names imported); iterate
    // children for `dotted_name` / `aliased_import` after the module.
    let mut past_module = module_range.is_none();
    for child in node.children() {
        let k = child.kind();
        let k: &str = k.as_ref();
        if !past_module {
            if Some(child.range()) == module_range {
                past_module = true;
            }
            continue;
        }
        match k {
            "dotted_name" => {
                let bare = String::from_utf8_lossy(&src[child.range()]).to_string();
                let module = if module_prefix.is_empty() {
                    bare.clone()
                } else {
                    format!("{}.{}", module_prefix, bare)
                };
                out.push(ImportBinding {
                    local: bare,
                    module,
                    line,
                });
            }
            "aliased_import" => {
                let name = child.field("name");
                let alias = child.field("alias");
                if let (Some(n), Some(a)) = (name, alias) {
                    let bare = String::from_utf8_lossy(&src[n.range()]).to_string();
                    let module = if module_prefix.is_empty() {
                        bare
                    } else {
                        format!("{}.{}", module_prefix, bare)
                    };
                    let local = String::from_utf8_lossy(&src[a.range()]).to_string();
                    out.push(ImportBinding { local, module, line });
                }
            }
            "wildcard_import" => { /* `from X import *` — no specific local */ }
            _ => {}
        }
    }
}
