use super::base::{collapse_ws, count_parse_errors, field_text, LanguageAdapter};
use crate::core::{CallKind, CallSite, Declaration, DeclarationKind, ParseResult};
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub struct RubyAdapter;

impl LanguageAdapter for RubyAdapter {
    fn language_name(&self) -> &'static str {
        "ruby"
    }

    fn parse<'a, D: Doc>(&self, path: &Path, source: &[u8], root: Node<'a, D>) -> ParseResult {
        let mut decls = Vec::new();
        _walk_module(&root, source, &mut decls);
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

fn _walk_module<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<Declaration>) {
    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        if let Some(decl) = _node_to_decl(&child, src) {
            out.push(decl);
        }
    }
}

fn _node_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let kind = node.kind();

    if kind == "module" {
        return _module_to_decl(node, src);
    } else if kind == "class" {
        return _class_to_decl(node, src);
    } else if kind == "method" {
        return _method_to_decl(node, src);
    } else if kind == "singleton_method" {
        return _singleton_method_to_decl(node, src);
    } else if kind == "call" {
        return _call_to_decl(node, src);
    } else if kind == "assignment" {
        // tree-sitter-ruby uses one `assignment` kind for both regular and
        // constant assignments — distinguish by the LHS shape.
        if node
            .field("left")
            .is_some_and(|l| l.kind() == "constant")
        {
            return _constant_to_decl(node, src);
        }
        return _assignment_to_decl(node, src);
    }

    None
}

fn _module_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name = _module_name(node, src)?;
    let body = node.field("body");
    let mut children = Vec::new();

    if let Some(b) = body {
        for child in b.children() {
            if !child.is_named() {
                continue;
            }
            if let Some(decl) = _node_to_decl(&child, src) {
                children.push(decl);
            }
        }
    }

    let sig = format!("module {}", name);
    let range = node.range();
    Some(Declaration {
        // Ruby modules are namespacing/mixin units, not classes — they
        // can't be instantiated. `Namespace` is the closest canonical kind;
        // `native_kind: "module"` preserves the source-true keyword for
        // outline/digest rendering.
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
        native_kind: Some("module".to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children,
        calls: Vec::new(),
    })
}

fn _class_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    // The `superclass` node spans the `<` operator as well as the name, so
    // its text is `< Root`. Only the name node is the supertype: taking the
    // whole node put an operator in `bases` and nothing ever matched it,
    // and taking the first named child put a comment there instead when one
    // sat between the operator and the name.
    let superclass = node
        .field("superclass")
        .and_then(|s| {
            s.children()
                .find(|c| matches!(c.kind().as_ref(), "constant" | "scope_resolution"))
        })
        .map(|c| collapse_ws(&c.text()).trim().to_string())
        .unwrap_or_default();

    let body = node.field("body");
    let mut children = Vec::new();

    if let Some(b) = body {
        // Ruby's `private` / `protected` / `public` are bare identifiers that
        // change the visibility scope for every method definition that
        // follows in the same body. Track the running state.
        let mut current_vis = String::new();
        for child in b.children() {
            if !child.is_named() {
                continue;
            }
            if child.kind() == "identifier" {
                let text = collapse_ws(&child.text()).trim().to_string();
                if matches!(text.as_str(), "private" | "protected" | "public") {
                    current_vis = if text == "public" { String::new() } else { text };
                    continue;
                }
            }
            if let Some(mut decl) = _node_to_decl(&child, src) {
                if decl.visibility.is_empty()
                    && matches!(decl.kind, DeclarationKind::Method)
                    && !current_vis.is_empty()
                {
                    decl.visibility = current_vis.clone();
                    // Reflect the visibility in the rendered signature so it
                    // shows up in outline/digest output.
                    decl.signature = format!("{} {}", current_vis, decl.signature);
                }
                children.push(decl);
            }
        }
    }

    let mut sig = format!("class {}", name);
    if !superclass.is_empty() {
        sig.push_str(" < ");
        sig.push_str(&superclass);
    }

    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Class,
        name: name.clone(),
        signature: sig,
        bases: if superclass.is_empty() {
            Vec::new()
        } else {
            vec![superclass]
        },
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
        visibility: String::new(),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: range.start,
        native_kind: Some("class".to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children,
        calls: Vec::new(),
    })
}

fn _method_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let params = _method_params(node, src);

    let sig = format!("def {}({})", name, params.join(", "));
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
        calls,
    })
}

fn _singleton_method_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<Declaration> {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let object = node
        .field("object")
        .map(|o| collapse_ws(&o.text()).trim().to_string())
        .unwrap_or_else(|| "?".to_string());
    let params = _method_params(node, src);

    let sig = format!("def {}.{}({})", object, name, params.join(", "));
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
        visibility: String::new(),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: range.start,
        native_kind: Some("singleton_method".to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls,
    })
}

fn _call_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<Declaration> {
    // Handle operator definitions like `define_method :+` or Rails associations
    let method = node
        .field("method")
        .map(|m| collapse_ws(&m.text()).trim().to_string())
        .unwrap_or_default();

    // attr_reader / attr_writer / attr_accessor — Ruby's idiomatic field
    // declaration. Surface as a single Field whose name lists every accessor.
    if matches!(
        method.as_str(),
        "attr_reader" | "attr_writer" | "attr_accessor"
    ) {
        let args = node
            .field("arguments")
            .map(|a| collapse_ws(&a.text()).trim().to_string())
            .unwrap_or_default();
        let sig = format!("{} {}", method, args);
        let range = node.range();
        return Some(Declaration {
            kind: DeclarationKind::Field,
            name: args,
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
            native_kind: Some(method),
            modifiers: Vec::new(),
            deprecated: false,
            children: Vec::new(),
            calls: Vec::new(),
        });
    }

    // Check for Rails association macros (has_many, belongs_to, etc.)
    let rails_associations = [
        "has_many",
        "belongs_to",
        "has_one",
        "has_and_belongs_to_many",
    ];
    if rails_associations.contains(&method.as_str()) {
        let args = node
            .field("arguments")
            .map(|a| collapse_ws(&a.text()).trim().to_string())
            .unwrap_or_default();
        let sig = format!("{} {}", method, args);
        let range = node.range();
        return Some(Declaration {
            kind: DeclarationKind::Field,
            name: args,
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
            native_kind: Some("association".to_string()),
            modifiers: Vec::new(),
            deprecated: false,
            children: Vec::new(),
            calls: Vec::new(),
        });
    }

    None
}

fn _assignment_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<Declaration> {
    let left = node.field("left")?;
    if left.kind() != "identifier" && left.kind() != "instance_variable" {
        return None;
    }
    let name = collapse_ws(&left.text()).trim().to_string();
    let sig = format!("{} = ...", name);
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
        native_kind: Some("assignment".to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _constant_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<Declaration> {
    let left = node.field("left")?;
    let name = collapse_ws(&left.text()).trim().to_string();
    let sig = format!("{} = ...", name);
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
        native_kind: Some("constant".to_string()),
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _module_name<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<String> {
    // Module name can be a constant or a scoped constant (e.g., Foo::Bar)
    let name_field = node.field("name")?;
    Some(collapse_ws(&name_field.text()).trim().to_string())
}

fn _method_params<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
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
        "method" | "singleton_method" | "class" | "module" | "lambda"
    ) {
        return;
    }

    if kind == "call" {
        if let Some(cs) = _call_site_from_call_ruby(node, src) {
            out.push(cs);
        }
    }

    for child in node.children() {
        _walk_calls_in_body(&child, src, out);
    }
}

fn _call_site_from_call_ruby<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<CallSite> {
    let method_node = node.field("method")?;
    let receiver_node = node.field("receiver");
    let name_raw = String::from_utf8_lossy(&src[method_node.range()]).to_string();
    let name = collapse_ws(&name_raw).trim().to_string();
    if name.is_empty() {
        return None;
    }

    let receiver_text = receiver_node
        .as_ref()
        .map(|r| collapse_ws(&String::from_utf8_lossy(&src[r.range()])));

    // Ruby idiom: `Foo.new(...)` is a constructor call. Classify as Construct
    // so callers/callees renderers can distinguish it from regular invocations
    // (mirrors the Construct edges produced by C++/C#/TS `new` expressions).
    let kind = if name == "new" {
        if let Some(rcv) = &receiver_text {
            if !rcv.is_empty() && _looks_like_constant_ruby(rcv) {
                CallKind::Construct
            } else {
                CallKind::Call
            }
        } else {
            CallKind::Call
        }
    } else {
        CallKind::Call
    };

    let (final_name, final_receiver) = if matches!(kind, CallKind::Construct) {
        // For `Foo.new`, the interesting target is `Foo` (the class), not the
        // generic `new` method. Mirror C++ `new Foo()` which records the type.
        (
            receiver_text
                .as_ref()
                .map(|r| _last_const_segment_ruby(r))
                .unwrap_or_else(|| name.clone()),
            None,
        )
    } else {
        (name, receiver_text)
    };

    let line = node.start_pos().line() as u32 + 1;
    Some(CallSite {
        name: final_name,
        receiver: final_receiver,
        line,
        kind,
    })
}

fn _looks_like_constant_ruby(text: &str) -> bool {
    text.chars()
        .next()
        .map(|c| c.is_ascii_uppercase())
        .unwrap_or(false)
}

fn _last_const_segment_ruby(text: &str) -> String {
    text.rsplit("::")
        .next()
        .unwrap_or(text)
        .trim()
        .to_string()
}
