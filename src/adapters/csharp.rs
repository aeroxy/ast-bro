use super::base::{collapse_ws, count_parse_errors, field_text, LanguageAdapter};
use crate::core::{CallKind, CallSite, Declaration, DeclarationKind, ParseResult};
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub struct CSharpAdapter;

impl LanguageAdapter for CSharpAdapter {
    fn language_name(&self) -> &'static str {
        "csharp"
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
    let mut file_scoped_ns: Option<Declaration> = None;

    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        let kind = child.kind();

        if kind == "namespace_declaration" {
            if let Some(ns) = file_scoped_ns.take() {
                out.push(ns);
            }
            out.push(_ns_to_decl(&child, src));
        } else if kind == "file_scoped_namespace_declaration" {
            if let Some(ns) = file_scoped_ns.take() {
                out.push(ns);
            }
            file_scoped_ns = Some(_ns_to_decl(&child, src));
        } else if _is_type_node(kind.as_ref()) {
            let type_decl = _type_to_decl(&child, src);
            if let Some(ns) = &mut file_scoped_ns {
                ns.end_line = type_decl.end_line;
                ns.end_byte = type_decl.end_byte;
                ns.children.push(type_decl);
            } else {
                out.push(type_decl);
            }
        } else if _is_member_node(kind.as_ref()) {
            if let Some(decl) = _member_to_decl(&child, src) {
                if let Some(ns) = &mut file_scoped_ns {
                    ns.children.push(decl);
                } else {
                    out.push(decl);
                }
            }
        }
    }

    if let Some(ns) = file_scoped_ns {
        out.push(ns);
    }
}

fn _is_type_node(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "record_declaration"
            | "record_struct_declaration"
            | "enum_declaration"
    )
}

fn _is_member_node(kind: &str) -> bool {
    matches!(
        kind,
        "method_declaration"
            | "constructor_declaration"
            | "destructor_declaration"
            | "property_declaration"
            | "indexer_declaration"
            | "event_declaration"
            | "event_field_declaration"
            | "field_declaration"
            | "delegate_declaration"
            | "operator_declaration"
            | "conversion_operator_declaration"
            | "enum_member_declaration"
    )
}

fn _type_node_kind(kind: &str) -> DeclarationKind {
    match kind {
        "class_declaration" => DeclarationKind::Class,
        "struct_declaration" => DeclarationKind::Struct,
        "interface_declaration" => DeclarationKind::Interface,
        "record_declaration" | "record_struct_declaration" => DeclarationKind::Record,
        "enum_declaration" => DeclarationKind::Enum,
        _ => DeclarationKind::Class,
    }
}

fn _member_node_kind(kind: &str) -> DeclarationKind {
    match kind {
        "method_declaration" => DeclarationKind::Method,
        "constructor_declaration" => DeclarationKind::Constructor,
        "destructor_declaration" => DeclarationKind::Destructor,
        "property_declaration" => DeclarationKind::Property,
        "indexer_declaration" => DeclarationKind::Indexer,
        "event_declaration" | "event_field_declaration" => DeclarationKind::Event,
        "field_declaration" => DeclarationKind::Field,
        "delegate_declaration" => DeclarationKind::Delegate,
        "operator_declaration" | "conversion_operator_declaration" => DeclarationKind::Operator,
        "enum_member_declaration" => DeclarationKind::EnumMember,
        _ => DeclarationKind::Field,
    }
}

fn _ns_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let name = field_text(node, "name").unwrap_or_default();
    let mut children = Vec::new();

    let body_node = node.field("body");
    let scope = body_node.as_ref().unwrap_or(node);

    for c in scope.children() {
        if !c.is_named() {
            continue;
        }
        let k = c.kind();
        if _is_type_node(k.as_ref()) {
            children.push(_type_to_decl(&c, src));
        } else if _is_member_node(k.as_ref()) {
            if let Some(m) = _member_to_decl(&c, src) {
                children.push(m);
            }
        } else if k == "namespace_declaration" || k == "file_scoped_namespace_declaration" {
            children.push(_ns_to_decl(&c, src));
        }
    }

    let range = node.range();
    Declaration {
        kind: DeclarationKind::Namespace,
        name: name.clone(),
        signature: format!("namespace {}", name),
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

fn _type_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Declaration {
    let kind = _type_node_kind(node.kind().as_ref());
    let name = field_text(node, "name").unwrap_or_else(|| "?".to_string());
    let bases = _base_types(node, src);
    let attrs = _attrs(node, src);
    let docs = _xml_docs(node);
    let visibility = _visibility(node, src, false, None);
    let signature = _type_signature(node, src);

    let mut children = Vec::new();
    if let Some(body) = node.field("body") {
        for c in body.children() {
            if !c.is_named() {
                continue;
            }
            let k = c.kind();
            if _is_type_node(k.as_ref()) {
                children.push(_type_to_decl(&c, src));
            } else if _is_member_node(k.as_ref()) {
                if let Some(m) = _member_to_decl(&c, src) {
                    children.push(m);
                }
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
        doc_start_byte: _leading_doc_start_byte(node).unwrap_or(range.start),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children,
        calls: Vec::new(),
    }
}

fn _member_to_decl<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<Declaration> {
    let kind = _member_node_kind(node.kind().as_ref());
    let name = _member_name(node, src)?;
    let attrs = _attrs(node, src);
    let docs = _xml_docs(node);
    let parent_kind = _parent_type_kind(node);
    let visibility = _visibility(node, src, true, parent_kind.as_deref());

    let signature = if node.kind() == "property_declaration" || node.kind() == "indexer_declaration"
    {
        _property_signature(node, src)
    } else {
        _member_signature_text(node, src)
    };
    let calls = if matches!(
        kind,
        DeclarationKind::Method
            | DeclarationKind::Constructor
            | DeclarationKind::Destructor
            | DeclarationKind::Operator
    ) {
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
        doc_start_byte: _leading_doc_start_byte(node).unwrap_or(range.start),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls,
    })
}

fn _type_signature<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let body = node.field("body");
    let end = body.map(|b| b.range().start).unwrap_or(node.range().end);
    let text = String::from_utf8_lossy(&src[node.range().start..end]).to_string();
    let text = _strip_leading_attrs(&text);
    collapse_ws(&text).trim_end_matches('{').trim().to_string()
}

fn _member_signature_text<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let mut end = node.range().end;
    for c in node.children() {
        let k = c.kind();
        if k == "block" || k == "arrow_expression_clause" || k == "accessor_list" {
            end = c.range().start;
            break;
        }
    }
    let text = String::from_utf8_lossy(&src[node.range().start..end]).to_string();
    let text = _strip_leading_attrs(&text);
    collapse_ws(&text)
        .trim_end_matches(&[' ', '{', '=', ';', '>'][..])
        .to_string()
}

fn _property_signature<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> String {
    let mut accessor_list = None;
    let mut expr_body = None;
    let mut head_end = None;

    for c in node.children() {
        let k = c.kind();
        if k == "accessor_list" {
            accessor_list = Some(c.clone());
            if head_end.is_none() {
                head_end = Some(c.range().start);
            }
            break;
        }
        if k == "arrow_expression_clause" {
            expr_body = Some(c.clone());
            if head_end.is_none() {
                head_end = Some(c.range().start);
            }
            break;
        }
    }

    let head =
        String::from_utf8_lossy(&src[node.range().start..head_end.unwrap_or(node.range().end)])
            .to_string();
    let head = _strip_leading_attrs(&head);
    let head = collapse_ws(&head)
        .trim_end_matches(&[' ', '{', '=', '>'][..])
        .to_string();

    if let Some(acc) = accessor_list {
        let mut accessors = Vec::new();
        for a in acc.children() {
            if a.kind() != "accessor_declaration" {
                continue;
            }
            let mut t = collapse_ws(&a.text());
            if let Some(idx) = t.find(" {") {
                t = t[..idx].to_string();
            } else if let Some(idx) = t.find(" =>") {
                t = t[..idx].to_string();
            } else if let Some(idx) = t.find(';') {
                t = t[..idx].to_string();
            }
            accessors.push(format!("{};", t.trim()));
        }
        return format!("{} {{ {} }}", head, accessors.join(" "));
    }

    if let Some(expr) = expr_body {
        let mut t = collapse_ws(&expr.text()).trim_end_matches(';').to_string();
        if !t.starts_with("=>") {
            t = format!("=> {}", t.trim_start_matches("=>").trim_start());
        }
        if t.len() > 80 {
            t = format!("{}...", &t[..77]);
        }
        return format!("{} {}", head, t);
    }

    head
}

fn _strip_leading_attrs(text: &str) -> String {
    let mut s = text.trim_start();
    while s.starts_with('[') {
        let mut depth = 0;
        let mut found = false;
        for (i, c) in s.char_indices() {
            if c == '[' {
                depth += 1;
            } else if c == ']' {
                depth -= 1;
                if depth == 0 {
                    s = s[i + 1..].trim_start();
                    found = true;
                    break;
                }
            }
        }
        if !found {
            break;
        }
    }
    s.to_string()
}

fn _attrs<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for c in node.children() {
        if c.kind() == "attribute_list" {
            out.push(collapse_ws(&c.text()));
        }
    }
    out
}

fn _xml_docs<'a, D: Doc>(node: &Node<'a, D>) -> Vec<String> {
    let mut docs = Vec::new();
    let mut sib = node.prev();
    while let Some(s) = sib {
        if s.kind() == "comment" {
            let t = s.text().into_owned();
            if t.starts_with("///") {
                docs.push(t);
            } else {
                break;
            }
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
        if s.kind() == "comment" && s.text().starts_with("///") {
            first = Some(s.clone());
            sib = s.prev();
        } else {
            break;
        }
    }
    first.map(|f| f.range().start)
}

fn _visibility<'a, D: Doc>(
    node: &Node<'a, D>,
    _src: &[u8],
    is_member: bool,
    parent_type_kind: Option<&str>,
) -> String {
    for c in node.children() {
        if c.kind() == "modifier" {
            let t = c.text().trim().to_string();
            if t == "public" || t == "protected" || t == "internal" || t == "private" {
                return t;
            }
        }
    }
    if !is_member {
        return "internal".to_string();
    }
    if let Some(p) = parent_type_kind {
        if p == "interface_declaration" || p == "enum_declaration" {
            return "public".to_string();
        }
    }
    "private".to_string()
}

fn _parent_type_kind<'a, D: Doc>(node: &Node<'a, D>) -> Option<String> {
    let p = node.parent()?;
    let g = p.parent()?;
    Some(g.kind().to_string())
}

fn _base_types<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Vec<String> {
    let base_list = node
        .field("bases")
        .or_else(|| node.children().find(|c| c.kind() == "base_list"));

    let mut out = Vec::new();
    if let Some(b) = base_list {
        for c in b.children() {
            if !c.is_named() {
                continue;
            }
            let t = collapse_ws(&c.text()).trim_end_matches(',').to_string();
            if !t.is_empty() {
                out.push(t);
            }
        }
    }
    out
}

fn _member_name<'a, D: Doc>(node: &Node<'a, D>, _src: &[u8]) -> Option<String> {
    let kind = node.kind();

    if matches!(
        kind.as_ref(),
        "method_declaration"
            | "property_declaration"
            | "event_declaration"
            | "delegate_declaration"
            | "indexer_declaration"
            | "constructor_declaration"
            | "destructor_declaration"
    ) {
        return field_text(node, "name");
    }

    if matches!(
        kind.as_ref(),
        "event_field_declaration" | "field_declaration"
    ) {
        if let Some(vd) = node.children().find(|c| c.kind() == "variable_declaration") {
            if let Some(decl) = vd.children().find(|c| c.kind() == "variable_declarator") {
                return field_text(&decl, "name");
            }
        }
    }

    if kind == "enum_member_declaration" {
        return field_text(node, "name");
    }

    if kind == "operator_declaration" {
        if let Some(op) = node.field("operator") {
            return Some(format!("operator{}", op.text()));
        }
        return Some("operator".to_string());
    }

    if kind == "conversion_operator_declaration" {
        if let Some(t) = node.field("type") {
            return Some(format!("operator_{}", t.text()));
        }
        return Some("operator".to_string());
    }

    None
}

// ---------------------------------------------------------------------------
// Call-site extraction
// ---------------------------------------------------------------------------

fn _extract_calls<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Vec<CallSite> {
    let mut out = Vec::new();
    let body = _member_body(node).unwrap_or_else(|| node.clone());
    _walk_calls_in_body(&body, src, &mut out);
    out
}

fn _member_body<'a, D: Doc>(node: &Node<'a, D>) -> Option<Node<'a, D>> {
    if let Some(b) = node.field("body") {
        return Some(b);
    }
    for c in node.children() {
        let k = c.kind();
        if k == "block" || k == "arrow_expression_clause" {
            return Some(c);
        }
    }
    None
}

fn _walk_calls_in_body<'a, D: Doc>(node: &Node<'a, D>, src: &[u8], out: &mut Vec<CallSite>) {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    if matches!(
        kind,
        "class_declaration"
            | "struct_declaration"
            | "interface_declaration"
            | "record_declaration"
            | "record_struct_declaration"
            | "enum_declaration"
            | "method_declaration"
            | "constructor_declaration"
            | "destructor_declaration"
            | "operator_declaration"
            | "conversion_operator_declaration"
            | "local_function_statement"
            | "lambda_expression"
            | "anonymous_method_expression"
    ) {
        return;
    }

    if kind == "invocation_expression" {
        if let Some(cs) = _call_site_from_invocation(node, src) {
            out.push(cs);
        }
    } else if kind == "object_creation_expression"
        || kind == "implicit_object_creation_expression"
    {
        if let Some(cs) = _call_site_from_object_creation(node, src) {
            out.push(cs);
        }
    }

    for child in node.children() {
        _walk_calls_in_body(&child, src, out);
    }
}

fn _call_site_from_invocation<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &[u8],
) -> Option<CallSite> {
    let func = node.field("function")?;
    let (name, receiver) = _split_callee(&func, src)?;
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

fn _split_callee<'a, D: Doc>(node: &Node<'a, D>, src: &[u8]) -> Option<(String, Option<String>)> {
    let kind = node.kind();
    let kind: &str = kind.as_ref();
    match kind {
        "identifier" | "generic_name" => {
            let raw = String::from_utf8_lossy(&src[node.range()]).to_string();
            Some((_strip_generics(&raw), None))
        }
        "member_access_expression" => {
            let object = node.field("expression");
            let name_node = node.field("name")?;
            let raw = String::from_utf8_lossy(&src[name_node.range()]).to_string();
            let recv = object
                .map(|o| collapse_ws(&String::from_utf8_lossy(&src[o.range()])));
            Some((_strip_generics(&raw), recv))
        }
        "qualified_name" => {
            let name_node = node.field("name")?;
            let qualifier = node.field("qualifier");
            let raw = String::from_utf8_lossy(&src[name_node.range()]).to_string();
            let recv = qualifier
                .map(|q| collapse_ws(&String::from_utf8_lossy(&src[q.range()])));
            Some((_strip_generics(&raw), recv))
        }
        "alias_qualified_name" => {
            let name_node = node.field("name")?;
            let raw = String::from_utf8_lossy(&src[name_node.range()]).to_string();
            Some((_strip_generics(&raw), None))
        }
        _ => {
            let raw = String::from_utf8_lossy(&src[node.range()]).to_string();
            Some((_strip_generics(&raw), None))
        }
    }
}

fn _strip_generics(text: &str) -> String {
    text.split('<').next().unwrap_or(text).trim().to_string()
}

fn _last_type_segment(text: &str) -> String {
    let head = text.split('<').next().unwrap_or(text).trim();
    head.rsplit('.').next().unwrap_or(head).trim().to_string()
}
