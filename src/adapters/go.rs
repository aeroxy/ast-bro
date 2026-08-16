use super::base::{collapse_ws, count_parse_errors, field_text, LanguageAdapter};
use crate::core::{
    CallKind, CallSite, Declaration, DeclarationGroup, DeclarationKind, ParseResult,
};
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub struct GoAdapter;

/// The node `tree-sitter-go` wraps a `var ( … )` block's specs in.
///
/// The parens live inside it rather than on the declaration, so it is a
/// group tell of its own, and reaching a var block's members means
/// descending through it. [`_group_context`] and [`_const_var_to_decls`]
/// both read this constant: spelled twice, a grammar rename leaves
/// detection working through the `(` child while the traversal stops
/// descending, emptying every parenthesised `var` block instead of
/// failing.
const VAR_SPEC_LIST: &str = "var_spec_list";

/// The `tree-sitter-go` node kinds that mark a `const` / `var` / `type`
/// declaration as a parenthesised group.
///
/// `const ( … )` and `type ( … )` keep their parens as direct children of
/// the declaration, so `"("` is the tell for both; only `var` interposes
/// a spec list.
///
/// Listing `const_spec_list` and `type_spec_list` here as well would read
/// as insurance against a grammar that starts interposing one for them,
/// and would not be: [`_const_var_to_decls`] reaches a block's members
/// through [`VAR_SPEC_LIST`] alone, so such a block would be classified
/// as a group and then yield nothing. What covers that upgrade is the
/// fixture, whose `const ( … )` and `type ( … )` blocks fail the suite
/// the moment their members stop being found.
const GROUP_KINDS: &[&str] = &["(", VAR_SPEC_LIST];

impl LanguageAdapter for GoAdapter {
    fn language_name(&self) -> &'static str {
        "go"
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
    let mut type_index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut pending_methods: Vec<(String, Declaration)> = Vec::new();

    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        let kind = child.kind();

        if kind == "package_clause" {
            let ns = _package_to_decl(&child, src);
            out.push(ns);
            package_ns = Some(out.last().unwrap().clone());
            continue;
        }
        if kind == "import_declaration" || kind == "comment" {
            continue;
        }
        if kind == "type_declaration" {
            for d in _type_declaration_to_decls(&child, src) {
                if matches!(d.kind, DeclarationKind::Struct | DeclarationKind::Interface) {
                    if let Some(ns) = &mut package_ns {
                        ns.children.push(d.clone());
                        type_index.insert(d.name.clone(), ns.children.len() - 1);
                    } else {
                        out.push(d.clone());
                        type_index.insert(d.name.clone(), out.len() - 1);
                    }
                } else if let Some(ns) = &mut package_ns {
                    ns.children.push(d);
                } else {
                    out.push(d);
                }
            }
            continue;
        }
        if kind == "function_declaration" {
            let func = _function_to_decl(&child, src);
            if let Some(ns) = &mut package_ns {
                ns.children.push(func);
            } else {
                out.push(func);
            }
            continue;
        }
        if kind == "method_declaration" {
            let recv = _receiver_type_name(&child, src);
            let decl = _method_to_decl(&child, src);
            if let Some(r) = recv {
                pending_methods.push((r, decl));
            } else if let Some(ns) = &mut package_ns {
                ns.children.push(decl);
            } else {
                out.push(decl);
            }
            continue;
        }
        if kind == "const_declaration" {
            let decls = _const_var_to_decls(&child, src, "const");
            if let Some(ns) = &mut package_ns {
                ns.children.extend(decls);
            } else {
                out.extend(decls);
            }
            continue;
        }
        if kind == "var_declaration" {
            let decls = _const_var_to_decls(&child, src, "var");
            if let Some(ns) = &mut package_ns {
                ns.children.extend(decls);
            } else {
                out.extend(decls);
            }
            continue;
        }
    }

    // Attach methods
    for (recv, method) in pending_methods {
        if let Some(&idx) = type_index.get(&recv) {
            let target = if let Some(ref mut inner) = package_ns {
                &mut inner.children[idx]
            } else {
                &mut out[idx]
            };
            target.end_line = std::cmp::max(target.end_line, method.end_line);
            target.end_byte = std::cmp::max(target.end_byte, method.end_byte);
            target.children.push(method);
        } else if let Some(ns) = &mut package_ns {
            ns.children.push(method);
        } else {
            out.push(method);
        }
    }

    if let Some(mut ns) = package_ns {
        if let Some(last) = ns.children.last() {
            ns.end_line = std::cmp::max(ns.end_line, last.end_line);
            ns.end_byte = std::cmp::max(ns.end_byte, last.end_byte);
        }
        // replace the first element which is the original empty package_ns
        out[0] = ns;
    }
}

fn _package_to_decl<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Declaration {
    let name_node = node.children().find(|c| c.kind() == "package_identifier");
    let name = name_node.map(|n| n.text().into_owned()).unwrap_or_default();

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
        children: Vec::new(),
        calls: Vec::new(),
    }
}

fn _type_declaration_to_decls<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Vec<Declaration> {
    let mut out = Vec::new();
    let (mut anchor, group) = _group_context(node);

    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        let d = match c.kind().as_ref() {
            "type_spec" => _type_spec_to_decl(&c, src, anchor.as_ref()),
            "type_alias" => _type_alias_to_decl(&c, src, anchor.as_ref()),
            _ => None,
        };
        if let Some(mut d) = d {
            // An ungrouped declaration holds exactly one spec, so the
            // anchor is spent on it: nothing else may claim the comment.
            anchor = None;
            d.group = group.clone();
            out.push(d);
        }
    }
    out
}

/// Split a `const` / `var` / `type` declaration's leading comment into the
/// two things it can be.
///
/// Above an ungrouped declaration, as in `// Standalone is not in a group.`
/// over `const Standalone = 1`, the comment sits on the declaration node
/// while the name sits on the spec inside it, so the spec has to reach out
/// to the declaration for its own doc comment. That is the returned anchor.
///
/// Above a group, the same comment documents the block rather than any
/// member of it, which is how `go doc` renders it. There is no anchor then,
/// and the comment travels as a `group` on every member instead of being
/// handed to whichever member happens to come first (issue #46).
///
/// Two grammar shapes spell a group, both listed in [`GROUP_KINDS`].
fn _group_context<'a, D: Doc>(
    node: &Node<'a, D>,
) -> (Option<Node<'a, D>>, Option<DeclarationGroup>) {
    let grouped = node
        .children()
        .any(|c| GROUP_KINDS.contains(&c.kind().as_ref()));
    if !grouped {
        return (Some(node.clone()), None);
    }
    let docs = _go_docs(node);
    let group = (!docs.is_empty()).then(|| DeclarationGroup {
        docs,
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
    });
    (None, group)
}

fn _type_spec_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    attach_outer_doc: Option<&Node<'a, D>>,
) -> Option<Declaration> {
    let name = field_text(node, "name")?;
    let type_node = node.field("type")?;

    let (docs, doc_start) = _spec_docs(node, attach_outer_doc);
    let visibility = _go_visibility(&name);
    let range = node.range();

    if type_node.kind() == "struct_type" {
        let (children, bases) = _struct_members_and_bases(&type_node, src);
        let mut signature = _slice_until(
            node.range().start,
            &type_node,
            src,
            "field_declaration_list",
            node,
        );
        if !signature.starts_with("type ") {
            signature = format!("type {}", signature);
        }

        return Some(Declaration {
            kind: DeclarationKind::Struct,
            name,
            signature,
            bases,
            attrs: Vec::new(),
            docs,
            docs_inside: false,
            group: None,
            visibility,
            start_line: node.start_pos().line() + 1,
            end_line: node.end_pos().line() + 1,
            start_byte: range.start,
            end_byte: range.end,
            doc_start_byte: doc_start,
            native_kind: None,
            modifiers: Vec::new(),
            deprecated: false,
            children,
            calls: Vec::new(),
        });
    }

    if type_node.kind() == "interface_type" {
        let (children, bases) = _interface_members_and_bases(&type_node, src);
        let mut signature = _slice_until_brace(node.range().start, &type_node, src, node);
        if !signature.starts_with("type ") {
            signature = format!("type {}", signature);
        }

        return Some(Declaration {
            kind: DeclarationKind::Interface,
            name,
            signature,
            bases,
            attrs: Vec::new(),
            docs,
            docs_inside: false,
            group: None,
            visibility,
            start_line: node.start_pos().line() + 1,
            end_line: node.end_pos().line() + 1,
            start_byte: range.start,
            end_byte: range.end,
            doc_start_byte: doc_start,
            native_kind: None,
            modifiers: Vec::new(),
            deprecated: false,
            children,
            calls: Vec::new(),
        });
    }

    let mut bases = Vec::new();
    let base_text = collapse_ws(&type_node.text());
    if !base_text.is_empty() {
        bases.push(base_text);
    }

    let mut sig = collapse_ws(&node.text());
    if !sig.starts_with("type ") {
        sig = format!("type {}", sig);
    }

    Some(Declaration {
        kind: DeclarationKind::Delegate,
        name,
        signature: sig,
        bases,
        attrs: Vec::new(),
        docs,
        docs_inside: false,
        group: None,
        visibility,
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: doc_start,
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _type_alias_to_decl<'a, D: Doc>(
    node: &Node<'a, D>,
    _src: &[u8],
    attach_outer_doc: Option<&Node<'a, D>>,
) -> Option<Declaration> {
    let name = field_text(node, "name")?;
    let (docs, doc_start) = _spec_docs(node, attach_outer_doc);

    let mut sig = collapse_ws(&node.text());
    if !sig.starts_with("type ") {
        sig = format!("type {}", sig);
    }

    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Delegate,
        name: name.clone(),
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs,
        docs_inside: false,
        group: None,
        visibility: _go_visibility(&name),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: doc_start,
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _struct_members_and_bases<'a, D: Doc>(
    struct_node: &Node<'a, D>,
    _src: &[u8],
) -> (Vec<Declaration>, Vec<String>) {
    let mut members = Vec::new();
    let mut bases = Vec::new();

    let body = struct_node
        .children()
        .find(|c| c.kind() == "field_declaration_list");
    if let Some(b) = body {
        for fd in b.children() {
            if fd.kind() != "field_declaration" {
                continue;
            }
            let mut ids = Vec::new();
            for c in fd.children() {
                if c.kind() == "field_identifier" {
                    ids.push(c.text().into_owned());
                }
            }
            if !ids.is_empty() {
                let first_name = ids[0].clone();
                let sig = collapse_ws(&fd.text());
                members.push(Declaration {
                    kind: DeclarationKind::Field,
                    name: first_name.clone(),
                    signature: sig,
                    bases: Vec::new(),
                    attrs: Vec::new(),
                    docs: _go_docs(&fd),
                    docs_inside: false,
                    group: None,
                    visibility: _go_visibility(&first_name),
                    start_line: fd.start_pos().line() + 1,
                    end_line: fd.end_pos().line() + 1,
                    start_byte: fd.range().start,
                    end_byte: fd.range().end,
                    doc_start_byte: _resolved_doc_start(&fd),
                    native_kind: None,
                    modifiers: Vec::new(),
                    deprecated: false,
                    children: Vec::new(),
                    calls: Vec::new(),
                });
            } else if let Some(base) = _embedded_base_name(&fd) {
                bases.push(base);
            }
        }
    }
    (members, bases)
}

fn _interface_members_and_bases<'a, D: Doc>(
    iface_node: &Node<'a, D>,
    _src: &[u8],
) -> (Vec<Declaration>, Vec<String>) {
    let mut members = Vec::new();
    let mut bases = Vec::new();

    for c in iface_node.children() {
        if c.kind() == "method_elem" {
            if let Some(name) = field_text(&c, "name") {
                let sig = collapse_ws(&c.text());
                members.push(Declaration {
                    kind: DeclarationKind::Method,
                    name: name.clone(),
                    signature: sig,
                    bases: Vec::new(),
                    attrs: Vec::new(),
                    docs: _go_docs(&c),
                    docs_inside: false,
                    group: None,
                    visibility: _go_visibility(&name),
                    start_line: c.start_pos().line() + 1,
                    end_line: c.end_pos().line() + 1,
                    start_byte: c.range().start,
                    end_byte: c.range().end,
                    doc_start_byte: _resolved_doc_start(&c),
                    native_kind: None,
                    modifiers: Vec::new(),
                    deprecated: false,
                    children: Vec::new(),
                    calls: Vec::new(),
                });
            }
        } else if c.kind() == "type_elem" {
            for cc in c.children() {
                if cc.kind() == "type_identifier" {
                    bases.push(cc.text().into_owned());
                    break;
                }
            }
        }
    }
    (members, bases)
}

fn _embedded_base_name<'a, D: Doc>(fd: &Node<'a, D>) -> Option<String> {
    for c in fd.children() {
        if !c.is_named() {
            continue;
        }
        if let Some(name) = _drill_to_type_identifier(&c) {
            return Some(name);
        }
        if matches!(c.kind().as_ref(), "qualified_type" | "generic_type") {
            return Some(collapse_ws(&c.text()));
        }
    }
    None
}

fn _function_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let docs = _go_docs(node);
    let body = node.field("body");
    let end = body.map(|b| b.range().start).unwrap_or(node.range().end);
    let sig = collapse_ws(&String::from_utf8_lossy(&src[node.range().start..end]))
        .trim_end_matches('{')
        .trim()
        .to_string();

    let calls = _extract_calls(node, src);
    let range = node.range();
    Declaration {
        kind: DeclarationKind::Function,
        name: name.clone(),
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs,
        docs_inside: false,
        group: None,
        visibility: _go_visibility(&name),
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

fn _method_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let docs = _go_docs(node);
    let body = node.field("body");
    let end = body.map(|b| b.range().start).unwrap_or(node.range().end);
    let sig = collapse_ws(&String::from_utf8_lossy(&src[node.range().start..end]))
        .trim_end_matches('{')
        .trim()
        .to_string();

    let calls = _extract_calls(node, src);
    let range = node.range();
    Declaration {
        kind: DeclarationKind::Method,
        name: name.clone(),
        signature: sig,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs,
        docs_inside: false,
        group: None,
        visibility: _go_visibility(&name),
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

fn _receiver_type_name<'a, D: Doc>(method: &Node<'a, D>, _src: &[u8]) -> Option<String> {
    let mut rcv = method.field("receiver");
    if rcv.is_none() {
        rcv = method.children().find(|c| c.kind() == "parameter_list");
    }
    let rcv = rcv?;

    for param in rcv.children() {
        if param.kind() != "parameter_declaration" {
            continue;
        }
        for c in param.children() {
            if let Some(name) = _drill_to_type_identifier(&c) {
                return Some(name);
            }
        }
    }
    None
}

fn _drill_to_type_identifier<'a, D: Doc>(node: &Node<'a, D>) -> Option<String> {
    let kind = node.kind();
    if kind == "type_identifier" {
        return Some(node.text().into_owned());
    }
    if kind == "pointer_type" {
        for c in node.children() {
            if !c.is_named() {
                continue;
            }
            if let Some(r) = _drill_to_type_identifier(&c) {
                return Some(r);
            }
        }
    }
    if kind == "generic_type" {
        for c in node.children() {
            if c.kind() == "type_identifier" {
                return Some(c.text().into_owned());
            }
        }
    }
    if kind == "qualified_type" {
        let mut last_id = None;
        for c in node.children() {
            if c.kind() == "type_identifier" {
                last_id = Some(c.text().into_owned());
            }
        }
        if last_id.is_some() {
            return last_id;
        }
    }
    None
}

fn _const_var_to_decls<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
    kind_name: &str,
) -> Vec<Declaration> {
    let mut out = Vec::new();
    let (mut anchor, group) = _group_context(node);

    let mut push = |spec: &Node<'a, D>, out: &mut Vec<Declaration>| {
        if let Some(mut d) = _spec_to_field(spec, src, kind_name, anchor.as_ref()) {
            // An ungrouped declaration holds exactly one spec, so the
            // anchor is spent on it: nothing else may claim the comment.
            anchor = None;
            d.group = group.clone();
            out.push(d);
        }
    };

    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        let k = c.kind();
        if k == "const_spec" || k == "var_spec" {
            push(&c, &mut out);
        } else if k == VAR_SPEC_LIST {
            for spec in c.children() {
                if spec.kind() == "var_spec" {
                    push(&spec, &mut out);
                }
            }
        }
    }
    out
}

fn _spec_to_field<'a, D: Doc>(
    node: &Node<'a, D>,
    _src: &[u8],
    kind_name: &str,
    outer_doc_anchor: Option<&Node<'a, D>>,
) -> Option<Declaration> {
    let name_node = node
        .field("name")
        .or_else(|| node.children().find(|c| c.kind() == "identifier"))?;
    let name = name_node.text().into_owned();

    let (docs, doc_start) = _spec_docs(node, outer_doc_anchor);

    let mut sig_text = collapse_ws(&node.text());
    if !sig_text.starts_with(&format!("{} ", kind_name)) && !sig_text.starts_with(kind_name) {
        sig_text = format!("{} {}", kind_name, sig_text);
    }

    let range = node.range();
    Some(Declaration {
        kind: DeclarationKind::Field,
        name: name.clone(),
        signature: sig_text.trim_end().to_string(),
        bases: Vec::new(),
        attrs: Vec::new(),
        docs,
        docs_inside: false,
        group: None,
        visibility: _go_visibility(&name),
        start_line: node.start_pos().line() + 1,
        end_line: node.end_pos().line() + 1,
        start_byte: range.start,
        end_byte: range.end,
        doc_start_byte: doc_start,
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _go_visibility(name: &str) -> String {
    if name.is_empty() {
        return "public".to_string();
    }
    let first = name.chars().next().unwrap();
    if first.is_uppercase() {
        "public".to_string()
    } else {
        "private".to_string()
    }
}

/// The comment group Go reads as `node`'s doc comment: the unbroken run of
/// comments on the lines directly above it, in source order.
///
/// A comment that starts on the line its previous sibling ends on is a
/// trailing comment on *that* sibling, not documentation for this one.
/// Struct fields are where the distinction earns its keep — `Alpha int //
/// in bytes` sits on the line directly above `Beta`, and without the check
/// every field in a densely annotated struct would inherit the comment of
/// the field before it.
fn _leading_comments<'a, D: Doc>(node: &Node<'a, D>) -> Vec<Node<'a, D>> {
    let mut out = Vec::new();
    let mut sib = node.prev();
    let mut last_start_line = node.start_pos().line();

    while let Some(s) = sib {
        if s.kind() != "comment" || s.end_pos().line() + 1 < last_start_line {
            break;
        }
        if let Some(p) = s.prev() {
            if p.end_pos().line() == s.start_pos().line() {
                break;
            }
        }
        last_start_line = s.start_pos().line();
        sib = s.prev();
        out.push(s);
    }
    out.reverse();
    out
}

fn _go_docs<'a, D: Doc>(node: &Node<'a, D>) -> Vec<String> {
    _leading_comments(node)
        .iter()
        .map(|c| c.text().into_owned())
        .collect()
}

fn _leading_doc_start_byte<'a, D: Doc>(node: &Node<'a, D>) -> Option<usize> {
    _leading_comments(node).first().map(|c| c.range().start)
}

/// A spec's doc comment, falling back to the enclosing declaration's.
///
/// `const Standalone = 1` puts the name on the spec and the comment above
/// the declaration that wraps it, so a spec with no comment of its own
/// asks the anchor `_group_context` handed it. A spec inside a group gets
/// no anchor and therefore keeps whatever comment is its own — including
/// none.
fn _spec_docs<'a, D: Doc>(
    node: &Node<'a, D>,
    outer_doc_anchor: Option<&Node<'a, D>>,
) -> (Vec<String>, usize) {
    let own = _leading_comments(node);
    let comments = match (own.is_empty(), outer_doc_anchor) {
        (true, Some(anchor)) => _leading_comments(anchor),
        _ => own,
    };
    let doc_start = comments
        .first()
        .map_or(node.range().start, |c| c.range().start);
    (
        comments.iter().map(|c| c.text().into_owned()).collect(),
        doc_start,
    )
}

fn _resolved_doc_start<'a, D: Doc>(node: &Node<'a, D>) -> usize {
    _leading_doc_start_byte(node).unwrap_or(node.range().start)
}

fn _slice_until<'a, D: Doc>(
    start_byte: usize,
    type_node: &Node<'a, D>,
    src: &[u8],
    body_node_type: &str,
    default_to_node: &Node<'a, D>,
) -> String {
    let mut cut = None;
    for c in type_node.children() {
        if c.kind() == body_node_type {
            cut = Some(c.range().start);
            break;
        }
    }
    let end = cut.unwrap_or(default_to_node.range().end);
    collapse_ws(&String::from_utf8_lossy(&src[start_byte..end]))
        .trim_end_matches('{')
        .trim()
        .to_string()
}

fn _slice_until_brace<'a, D: Doc>(
    start_byte: usize,
    type_node: &Node<'a, D>,
    src: &[u8],
    default_to_node: &Node<'a, D>,
) -> String {
    let mut cut = None;
    for c in type_node.children() {
        if c.kind() == "{" {
            cut = Some(c.range().start);
            break;
        }
    }
    let end = cut.unwrap_or(default_to_node.range().end);
    collapse_ws(&String::from_utf8_lossy(&src[start_byte..end]))
        .trim_end_matches('{')
        .trim()
        .to_string()
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
        "function_declaration" | "method_declaration" | "func_literal"
    ) {
        return;
    }

    if kind == "call_expression" {
        if let Some(cs) = _call_site_from_call_go(node, src) {
            out.push(cs);
        }
    }

    for child in node.children() {
        _walk_calls_in_body(&child, src, out);
    }
}

fn _call_site_from_call_go<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<CallSite> {
    let func = node.field("function")?;
    let (name, receiver, kind) = _split_callee_go(&func, src)?;
    let line = node.start_pos().line() as u32 + 1;
    Some(CallSite {
        name,
        receiver,
        line,
        kind,
    })
}

fn _split_callee_go<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<(String, Option<String>, CallKind)> {
    let kind = node.kind();
    let kind_ref: &str = kind.as_ref();
    match kind_ref {
        "identifier" => {
            let text = String::from_utf8_lossy(&src[node.range()]).to_string();
            // Built-in `new(T)` is a constructor-like call in Go; tree-sitter
            // models it as a regular call_expression with `function = identifier "new"`.
            // Leave classification as Call — Go has no per-type constructor.
            Some((text, None, CallKind::Call))
        }
        "selector_expression" => {
            let field = node.field("field")?;
            let operand = node.field("operand");
            let name = String::from_utf8_lossy(&src[field.range()]).to_string();
            let receiver = operand.map(|o| collapse_ws(&String::from_utf8_lossy(&src[o.range()])));
            Some((name, receiver, CallKind::Call))
        }
        "index_expression" | "parenthesized_expression" => {
            let inner = node
                .field("operand")
                .or_else(|| node.children().find(|c| c.is_named()))?;
            _split_callee_go(&inner, src)
        }
        _ => {
            let raw = String::from_utf8_lossy(&src[node.range()]).to_string();
            let collapsed = collapse_ws(&raw);
            if collapsed.is_empty() {
                return None;
            }
            let name = collapsed
                .rsplit('.')
                .next()
                .unwrap_or(&collapsed)
                .split('[')
                .next()
                .unwrap_or(&collapsed)
                .trim()
                .to_string();
            if name.is_empty() {
                return None;
            }
            Some((name, None, CallKind::Call))
        }
    }
}
