//! Elixir adapter.
//!
//! tree-sitter-elixir has no declaration node kinds: `defmodule`, `def`,
//! `defp`, `alias`, … are all generic `call` nodes whose `target` field is
//! an `identifier` naming the macro, and a module attribute is a
//! `unary_operator` (`@`) wrapping a `call` (`@doc "…"`) or a bare
//! `identifier` (`@doc` with no value). Declarations are therefore
//! recognised by the target name, never by node kind:
//!
//! ```text
//! call                                  defmodule Foo.Bar do … end
//!   target: identifier "defmodule"
//!   arguments
//!     alias "Foo.Bar"
//!   do_block                            module body, one node per form
//! call                                  def add(a, b) when a > 0, do: a + b
//!   target: identifier "def"
//!   arguments
//!     binary_operator (when)            head with a guard
//!       left: call "add(a, b)"          name + parameter list
//!     keywords "do: a + b"              keyword-form body
//! unary_operator (@)                    @spec add(integer, integer) :: integer
//!   operand: call
//!     target: identifier "spec"
//!     arguments
//!       binary_operator (::)
//! ```
//!
//! Mapping: `defmodule` → `Class` (`Struct` when the body has `defstruct` /
//! `defexception`), `defprotocol` → `Interface`, `defimpl` → `Class` whose
//! base is the protocol. Functions are `Function`s; the clauses of one
//! name/arity that sit next to each other are folded into a single entry
//! spanning every clause. `@doc` / `@moduledoc` become docs, `@spec`,
//! `@impl` and `@deprecated` ride as `attrs` on the next definition, and
//! `@behaviour` lands in the module's `bases` so `implements` can follow it.

use super::base::{collapse_ws, count_parse_errors, LanguageAdapter};
use crate::core::{Declaration, DeclarationKind, ImportBinding, ParseResult};
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub struct ElixirAdapter;

impl LanguageAdapter for ElixirAdapter {
    fn language_name(&self) -> &'static str {
        "elixir"
    }

    fn parse<'a, D: Doc>(&self, path: &Path, source: &[u8], root: Node<'a, D>) -> ParseResult {
        let mut walker = Walker {
            src: source,
            imports: Vec::new(),
        };
        // A script (`.exs`) may hold top-level forms outside any module, so
        // the file root is walked with the same body walker as a module.
        let body = walker.walk_body(&root, "");
        ParseResult {
            path: path.to_path_buf(),
            language: self.language_name(),
            source: source.to_vec(),
            line_count: source.iter().filter(|&&b| b == b'\n').count() + 1,
            declarations: body.decls,
            error_count: count_parse_errors(root.clone()),
            imports: walker.imports,
        }
    }
}

/// Function-definition macros and what each one means for the declaration.
/// `(keyword, private, modifier)`.
const DEF_MACROS: &[(&str, bool, Option<&str>)] = &[
    ("def", false, None),
    ("defp", true, None),
    ("defmacro", false, Some("macro")),
    ("defmacrop", true, Some("macro")),
    ("defguard", false, Some("guard")),
    ("defguardp", true, Some("guard")),
    ("defdelegate", false, Some("delegate")),
];

/// Attributes that annotate the *next* definition rather than declaring a
/// module attribute of their own.
const ANNOTATION_ATTRS: &[&str] = &["spec", "impl", "deprecated"];

/// Longest attribute value / type body kept verbatim in a signature; longer
/// ones are elided so a multi-line map literal cannot flood a `map` line.
const MAX_INLINE_VALUE: usize = 60;
const MAX_INLINE_TYPE: usize = 100;

struct Walker<'s> {
    src: &'s [u8],
    imports: Vec<ImportBinding>,
}

/// What a module body yields besides its child declarations.
#[derive(Default)]
struct Body {
    decls: Vec<Declaration>,
    moduledoc: Vec<String>,
    moduledoc_hidden: bool,
    behaviours: Vec<String>,
    defines_struct: bool,
}

/// `@doc` / `@spec` / … seen since the last definition, waiting to attach
/// to the next one.
#[derive(Default)]
struct Pending {
    /// Byte / line where the first pending attribute starts — the
    /// definition's range is widened back to it, so `show` returns the
    /// `@doc` and `@spec` along with the clauses.
    start: Option<(usize, usize)>,
    docs: Vec<String>,
    hidden: bool,
    attrs: Vec<String>,
    deprecated: bool,
    impl_: bool,
}

impl Pending {
    fn mark_start<'a, D: Doc>(&mut self, node: &Node<'a, D>) {
        if self.start.is_none() {
            self.start = Some((node.range().start, node.start_pos().line() + 1));
        }
    }
}

/// The function most recently pushed, so the next clause of the same
/// name/arity can be folded into it.
struct LastFn {
    keyword: String,
    name: String,
    arity: usize,
}

impl<'s> Walker<'s> {
    fn walk_body<'a, D: Doc>(&mut self, body: &Node<'a, D>, module_fqn: &str) -> Body {
        let mut out = Body::default();
        let mut pending = Pending::default();
        let mut typedoc: Vec<String> = Vec::new();
        let mut last_fn: Option<LastFn> = None;

        for child in body.children() {
            if !child.is_named() || child.kind() == "comment" {
                continue;
            }
            if child.kind() == "unary_operator" {
                let Some((attr, value)) = _attribute(&child) else {
                    continue;
                };
                match attr.as_str() {
                    "moduledoc" => match value.as_ref().map(|v| v.kind()) {
                        Some(k) if k == "boolean" => {
                            out.moduledoc_hidden = _text(value.as_ref().unwrap()) == "false"
                        }
                        Some(_) => out.moduledoc = self.doc_lines(&child),
                        None => {}
                    },
                    "doc" => {
                        pending.mark_start(&child);
                        match value.as_ref().map(|v| v.kind()) {
                            Some(k) if k == "boolean" => {
                                pending.hidden = _text(value.as_ref().unwrap()) == "false"
                            }
                            // `@doc since: "1.2"` is metadata, not text.
                            Some(k) if k == "keywords" => {}
                            Some(_) => pending.docs = self.doc_lines(&child),
                            None => {}
                        }
                    }
                    "typedoc" => {
                        if value.as_ref().is_some_and(|v| v.kind() != "boolean") {
                            typedoc = self.doc_lines(&child);
                        }
                    }
                    "behaviour" | "behavior" => {
                        if let Some(v) = &value {
                            out.behaviours.push(_text(v));
                        }
                    }
                    a if ANNOTATION_ATTRS.contains(&a) => {
                        pending.mark_start(&child);
                        pending.attrs.push(_text(&child));
                        pending.deprecated |= a == "deprecated";
                        pending.impl_ |= a == "impl";
                    }
                    "callback" | "macrocallback" => {
                        last_fn = None;
                        let decl = self.callback_decl(&child, &attr, value.as_ref(), &mut pending);
                        out.decls.extend(decl);
                    }
                    "type" | "typep" | "opaque" => {
                        last_fn = None;
                        let docs = std::mem::take(&mut typedoc);
                        out.decls
                            .extend(self.type_decl(&child, &attr, value.as_ref(), docs));
                    }
                    _ => {
                        last_fn = None;
                        out.decls
                            .push(self.attribute_decl(&child, &attr, value.as_ref()));
                    }
                }
                continue;
            }
            if child.kind() != "call" {
                continue;
            }
            let Some(keyword) = _call_keyword(&child) else {
                continue;
            };
            match keyword.as_str() {
                "defmodule" | "defprotocol" | "defimpl" => {
                    last_fn = None;
                    pending = Pending::default();
                    if let Some(decl) = self.module_decl(&child, &keyword, module_fqn) {
                        out.decls.push(decl);
                    }
                }
                k if DEF_MACROS.iter().any(|(kw, _, _)| *kw == k) => {
                    let p = std::mem::take(&mut pending);
                    self.function_clause(&child, &keyword, p, &mut out.decls, &mut last_fn);
                }
                "defstruct" | "defexception" => {
                    last_fn = None;
                    out.defines_struct = true;
                    out.decls.push(self.struct_decl(&child, &keyword));
                }
                "alias" | "import" | "require" | "use" => {
                    self.import(&child, &keyword);
                }
                // Any other macro call (`test`, `plug`, `defoverridable`, …)
                // is not a definition; a dangling `@doc` must not leak past
                // it onto the next one.
                _ => pending = Pending::default(),
            }
        }
        out
    }

    fn module_decl<'a, D: Doc>(
        &mut self,
        node: &Node<'a, D>,
        keyword: &str,
        parent_fqn: &str,
    ) -> Option<Declaration> {
        let args = _named_args(node);
        let first = args.first()?;
        let written = _text(first);
        let for_type = _keyword_value(&args, "for").map(|v| _text(&v));

        let (kind, name, signature, mut bases, native) = match keyword {
            "defprotocol" => (
                DeclarationKind::Interface,
                written.clone(),
                format!("defprotocol {}", written),
                Vec::new(),
                "protocol",
            ),
            "defimpl" => {
                // `defimpl P, for: T` defines `P.T`; without `for:` it
                // implements the protocol for the enclosing module.
                let target = for_type.clone().unwrap_or_else(|| parent_fqn.to_string());
                let name = if target.is_empty() || target.starts_with('[') {
                    written.clone()
                } else {
                    format!("{}.{}", written, target)
                };
                let args_text = node
                    .children()
                    .find(|c| c.kind() == "arguments")
                    .map(|a| _text(&a))
                    .unwrap_or_else(|| written.clone());
                (
                    DeclarationKind::Class,
                    name,
                    format!("defimpl {}", args_text),
                    vec![written.clone()],
                    "impl",
                )
            }
            _ => (
                DeclarationKind::Class,
                written.clone(),
                format!("defmodule {}", written),
                Vec::new(),
                "module",
            ),
        };

        let fqn = if parent_fqn.is_empty() {
            written.clone()
        } else {
            format!("{}.{}", parent_fqn, written)
        };
        let body = match node.children().find(|c| c.kind() == "do_block") {
            Some(b) => self.walk_body(&b, &fqn),
            None => Body::default(),
        };
        bases.extend(body.behaviours);
        let kind = if body.defines_struct && kind == DeclarationKind::Class && native == "module" {
            DeclarationKind::Struct
        } else {
            kind
        };
        let mut modifiers = Vec::new();
        if body.moduledoc_hidden {
            modifiers.push("hidden".to_string());
        }

        let range = node.range();
        Some(Declaration {
            kind,
            name,
            signature,
            bases,
            attrs: Vec::new(),
            docs: body.moduledoc,
            docs_inside: true,
            visibility: String::new(),
            start_line: node.start_pos().line() + 1,
            end_line: node.end_pos().line() + 1,
            start_byte: range.start,
            end_byte: range.end,
            doc_start_byte: range.start,
            native_kind: Some(native.to_string()),
            modifiers,
            deprecated: false,
            children: body.decls,
            calls: Vec::new(),
        })
    }

    /// One `def`-family clause: either a new declaration or, when it
    /// continues the previous clause's name/arity, an extension of it.
    fn function_clause<'a, D: Doc>(
        &mut self,
        node: &Node<'a, D>,
        keyword: &str,
        pending: Pending,
        out: &mut Vec<Declaration>,
        last_fn: &mut Option<LastFn>,
    ) {
        let args = _named_args(node);
        let Some(head) = args.first() else {
            return;
        };
        let (name, arity) = _head_name_arity(head);
        let range = node.range();
        let end_line = node.end_pos().line() + 1;

        let continues = last_fn
            .as_ref()
            .is_some_and(|l| l.keyword == keyword && l.name == name && l.arity == arity);
        if continues {
            if let Some(prev) = out.last_mut() {
                prev.end_line = end_line;
                prev.end_byte = range.end;
                for a in pending.attrs {
                    if !prev.attrs.contains(&a) {
                        prev.attrs.push(a);
                    }
                }
                if prev.docs.is_empty() {
                    prev.docs = pending.docs;
                }
                prev.deprecated |= pending.deprecated;
                return;
            }
        }

        let (_, private, modifier) = DEF_MACROS
            .iter()
            .find(|(kw, _, _)| *kw == keyword)
            .copied()
            .unwrap_or(("def", false, None));
        // `defdelegate` keeps its `to:` target in the signature; every other
        // macro shows only the head (the `do:` body is not signature).
        let signature = if keyword == "defdelegate" {
            let args_text = node
                .children()
                .find(|c| c.kind() == "arguments")
                .map(|a| _text(&a))
                .unwrap_or_default();
            format!("{} {}", keyword, args_text)
        } else {
            format!("{} {}", keyword, _text(head))
        };
        let mut modifiers = Vec::new();
        if let Some(m) = modifier {
            modifiers.push(m.to_string());
        }
        if pending.impl_ {
            modifiers.push("impl".to_string());
        }
        if pending.hidden {
            modifiers.push("hidden".to_string());
        }
        let (start_byte, start_line) = pending
            .start
            .unwrap_or((range.start, node.start_pos().line() + 1));

        *last_fn = Some(LastFn {
            keyword: keyword.to_string(),
            name: name.clone(),
            arity,
        });
        out.push(Declaration {
            kind: DeclarationKind::Function,
            name,
            signature,
            bases: Vec::new(),
            attrs: pending.attrs,
            docs: pending.docs,
            docs_inside: false,
            visibility: if private {
                "private".to_string()
            } else {
                String::new()
            },
            start_line,
            end_line,
            start_byte,
            end_byte: range.end,
            doc_start_byte: start_byte,
            native_kind: Some(keyword.to_string()),
            modifiers,
            deprecated: pending.deprecated,
            children: Vec::new(),
            calls: Vec::new(),
        });
    }

    /// `@callback name(args) :: ret` — a behaviour's abstract function.
    fn callback_decl<'a, D: Doc>(
        &self,
        node: &Node<'a, D>,
        attr: &str,
        value: Option<&Node<'a, D>>,
        pending: &mut Pending,
    ) -> Option<Declaration> {
        let value = value?;
        let p = std::mem::take(pending);
        let (name, _) = _head_name_arity(&_spec_head(value));
        let range = node.range();
        let (start_byte, start_line) = p
            .start
            .unwrap_or((range.start, node.start_pos().line() + 1));
        let mut modifiers = vec!["callback".to_string()];
        if attr == "macrocallback" {
            modifiers.push("macro".to_string());
        }
        if p.hidden {
            modifiers.push("hidden".to_string());
        }
        Some(Declaration {
            kind: DeclarationKind::Function,
            name,
            signature: _text(node),
            bases: Vec::new(),
            attrs: p.attrs,
            docs: p.docs,
            docs_inside: false,
            visibility: String::new(),
            start_line,
            end_line: node.end_pos().line() + 1,
            start_byte,
            end_byte: range.end,
            doc_start_byte: start_byte,
            native_kind: Some(attr.to_string()),
            modifiers,
            deprecated: p.deprecated,
            children: Vec::new(),
            calls: Vec::new(),
        })
    }

    /// `@type` / `@typep` / `@opaque` — surfaced as a type-alias field.
    fn type_decl<'a, D: Doc>(
        &self,
        node: &Node<'a, D>,
        attr: &str,
        value: Option<&Node<'a, D>>,
        docs: Vec<String>,
    ) -> Option<Declaration> {
        let value = value?;
        let (name, _) = _head_name_arity(&_spec_head(value));
        let full = _text(node);
        let signature = if full.len() <= MAX_INLINE_TYPE {
            full
        } else {
            format!("@{} {} :: ...", attr, _text(&_spec_head(value)))
        };
        let range = node.range();
        Some(Declaration {
            kind: DeclarationKind::Field,
            name,
            signature,
            bases: Vec::new(),
            attrs: Vec::new(),
            docs,
            docs_inside: false,
            visibility: if attr == "typep" {
                "private".to_string()
            } else {
                String::new()
            },
            start_line: node.start_pos().line() + 1,
            end_line: node.end_pos().line() + 1,
            start_byte: range.start,
            end_byte: range.end,
            doc_start_byte: range.start,
            native_kind: Some(attr.to_string()),
            modifiers: Vec::new(),
            deprecated: false,
            children: Vec::new(),
            calls: Vec::new(),
        })
    }

    /// Any other `@name value` — a compile-time module constant, so private
    /// to the module.
    fn attribute_decl<'a, D: Doc>(
        &self,
        node: &Node<'a, D>,
        attr: &str,
        value: Option<&Node<'a, D>>,
    ) -> Declaration {
        let signature = match value {
            Some(v) => {
                let v = _text(v);
                if v.len() <= MAX_INLINE_VALUE {
                    format!("@{} {}", attr, v)
                } else {
                    format!("@{} ...", attr)
                }
            }
            None => format!("@{}", attr),
        };
        let range = node.range();
        Declaration {
            kind: DeclarationKind::Field,
            name: attr.to_string(),
            signature,
            bases: Vec::new(),
            attrs: Vec::new(),
            docs: Vec::new(),
            docs_inside: false,
            visibility: "private".to_string(),
            start_line: node.start_pos().line() + 1,
            end_line: node.end_pos().line() + 1,
            start_byte: range.start,
            end_byte: range.end,
            doc_start_byte: range.start,
            native_kind: Some("attribute".to_string()),
            modifiers: Vec::new(),
            deprecated: false,
            children: Vec::new(),
            calls: Vec::new(),
        }
    }

    /// `defstruct [...]` / `defexception [...]` — one field naming the
    /// struct's keys.
    fn struct_decl<'a, D: Doc>(&self, node: &Node<'a, D>, keyword: &str) -> Declaration {
        let full = _text(node);
        let signature = if full.len() <= MAX_INLINE_TYPE {
            full
        } else {
            let keys = _struct_keys(node);
            format!("{} [{}]", keyword, keys.join(", "))
        };
        let range = node.range();
        Declaration {
            kind: DeclarationKind::Field,
            name: keyword.to_string(),
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
            native_kind: Some(keyword.to_string()),
            modifiers: Vec::new(),
            deprecated: false,
            children: Vec::new(),
            calls: Vec::new(),
        }
    }

    /// `alias` / `import` / `require` / `use`, including the multi form
    /// `alias Foo.{Bar, Baz}`. `alias` binds the last segment (or `as:`);
    /// the others are referenced by their full name.
    fn import<'a, D: Doc>(&mut self, node: &Node<'a, D>, keyword: &str) {
        let args = _named_args(node);
        let Some(first) = args.first() else {
            return;
        };
        let line = node.start_pos().line() as u32 + 1;
        let as_name = _keyword_value(&args, "as").map(|v| _text(&v));

        let mut modules = Vec::new();
        if first.kind() == "dot" {
            if let (Some(left), Some(right)) = (first.field("left"), first.field("right")) {
                if right.kind() == "tuple" {
                    let prefix = _text(&left);
                    for m in right
                        .children()
                        .filter(|c| c.is_named() && c.kind() != "comment")
                    {
                        modules.push(format!("{}.{}", prefix, _text(&m)));
                    }
                }
            }
        }
        if modules.is_empty() {
            modules.push(_text(first));
        }

        for module in modules {
            let local = if keyword == "alias" {
                as_name
                    .clone()
                    .unwrap_or_else(|| module.rsplit('.').next().unwrap_or(&module).to_string())
            } else {
                module.clone()
            };
            self.imports.push(ImportBinding {
                local,
                module,
                line,
            });
        }
    }

    /// The doc attribute's source lines, dedented to the attribute's own
    /// column so a heredoc reads the way it was written.
    fn doc_lines<'a, D: Doc>(&self, node: &Node<'a, D>) -> Vec<String> {
        let start = node.range().start;
        let line_start = self.src[..start]
            .iter()
            .rposition(|&b| b == b'\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        let col = start - line_start;
        let text = node.text();
        text.lines()
            .enumerate()
            .map(|(i, l)| {
                if i == 0 {
                    l.to_string()
                } else {
                    let indent = l.len() - l.trim_start_matches(' ').len();
                    l[indent.min(col)..].to_string()
                }
            })
            .collect()
    }
}

/// `node.text()` with whitespace runs collapsed — multi-line heads and
/// values read as one line.
fn _text<'a, D: Doc>(node: &Node<'a, D>) -> String {
    collapse_ws(&node.text())
}

/// The macro name of a `call` whose target is a bare identifier
/// (`def`, `defmodule`, `alias`, …). Remote calls (`Foo.bar`) have a `dot`
/// target and are not declarations.
fn _call_keyword<'a, D: Doc>(node: &Node<'a, D>) -> Option<String> {
    let target = node.field("target")?;
    if target.kind() != "identifier" {
        return None;
    }
    Some(target.text().into_owned())
}

/// Named, non-comment children of a call's `arguments` node.
fn _named_args<'a, D: Doc>(node: &Node<'a, D>) -> Vec<Node<'a, D>> {
    node.children()
        .find(|c| c.kind() == "arguments")
        .map(|a| {
            a.children()
                .filter(|c| c.is_named() && c.kind() != "comment")
                .collect()
        })
        .unwrap_or_default()
}

/// Value of `key:` in a call's trailing keyword list (`for: T`, `as: A`).
fn _keyword_value<'a, D: Doc>(args: &[Node<'a, D>], key: &str) -> Option<Node<'a, D>> {
    let kws = args.iter().find(|a| a.kind() == "keywords")?;
    for pair in kws.children().filter(|c| c.kind() == "pair") {
        let k = pair.field("key")?;
        if k.text().trim().trim_end_matches(':') == key {
            return pair.field("value");
        }
    }
    None
}

/// `@name value` → `(name, Some(value))`; a bare `@name` → `(name, None)`.
/// `None` for any unary operator other than `@`.
fn _attribute<'a, D: Doc>(node: &Node<'a, D>) -> Option<(String, Option<Node<'a, D>>)> {
    let op = node.field("operator")?;
    if op.text() != "@" {
        return None;
    }
    let operand = node.field("operand")?;
    match operand.kind().as_ref() {
        "identifier" => Some((operand.text().into_owned(), None)),
        "call" => {
            let name = _call_keyword(&operand)?;
            let value = _named_args(&operand).into_iter().next();
            Some((name, value))
        }
        _ => None,
    }
}

/// Strip a typespec down to its head: `f(a) :: r when a: t` → `f(a)`.
fn _spec_head<'a, D: Doc>(node: &Node<'a, D>) -> Node<'a, D> {
    let mut cur = node.clone();
    loop {
        if cur.kind() != "binary_operator" {
            return cur;
        }
        let op = cur.field("operator").map(|o| o.text().into_owned());
        match op.as_deref() {
            Some("when") | Some("::") => match cur.field("left") {
                Some(l) => cur = l,
                None => return cur,
            },
            _ => return cur,
        }
    }
}

/// Name and arity of a definition head. Covers `name(args)`, a bare
/// zero-arity `name`, a guarded head (`… when …`), operator definitions
/// (`a <> b`, `-a`) and unquoted names (`unquote(n)(args)`).
fn _head_name_arity<'a, D: Doc>(head: &Node<'a, D>) -> (String, usize) {
    match head.kind().as_ref() {
        "binary_operator" => {
            let op = head
                .field("operator")
                .map(|o| o.text().into_owned())
                .unwrap_or_default();
            if op == "when" {
                if let Some(left) = head.field("left") {
                    return _head_name_arity(&left);
                }
            }
            (op, 2)
        }
        "unary_operator" => {
            let op = head
                .field("operator")
                .map(|o| o.text().into_owned())
                .unwrap_or_default();
            (op, 1)
        }
        "call" => {
            let name = head
                .field("target")
                .map(|t| _text(&t))
                .unwrap_or_else(|| "?".to_string());
            let arity = _named_args(head).len();
            (name, arity)
        }
        _ => (_text(head), 0),
    }
}

/// Keys of `defstruct [:a, b: 1]` / `defstruct a: 1` — atoms and keyword
/// keys, in source order.
fn _struct_keys<'a, D: Doc>(node: &Node<'a, D>) -> Vec<String> {
    let mut keys = Vec::new();
    let args = _named_args(node);
    let mut stack: Vec<Node<'a, D>> = args.into_iter().rev().collect();
    while let Some(n) = stack.pop() {
        match n.kind().as_ref() {
            "list" | "keywords" => {
                let mut kids: Vec<_> = n.children().filter(|c| c.is_named()).collect();
                kids.reverse();
                stack.extend(kids);
            }
            "atom" => keys.push(n.text().trim_start_matches(':').to_string()),
            "pair" => {
                if let Some(k) = n.field("key") {
                    keys.push(k.text().trim().trim_end_matches(':').to_string());
                }
            }
            _ => {}
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use ast_grep_language::{LanguageExt, SupportLang};

    fn parse(src: &str) -> ParseResult {
        let grep = SupportLang::Elixir.ast_grep(src);
        ElixirAdapter.parse(Path::new("x.ex"), src.as_bytes(), grep.root())
    }

    #[test]
    fn directives_become_import_bindings() {
        let r = parse(
            "defmodule A do\n  alias Foo.{Bar, Baz.Qux}\n  alias Foo.Long, as: L\n  import Enum, only: [map: 2]\n  require Logger\n  use GenServer\nend\n",
        );
        let got: Vec<(&str, &str, u32)> = r
            .imports
            .iter()
            .map(|i| (i.local.as_str(), i.module.as_str(), i.line))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Bar", "Foo.Bar", 2),
                ("Qux", "Foo.Baz.Qux", 2),
                ("L", "Foo.Long", 3),
                ("Enum", "Enum", 4),
                ("Logger", "Logger", 5),
                ("GenServer", "GenServer", 6),
            ]
        );
    }

    #[test]
    fn heads_name_operators_and_zero_arity_forms() {
        let r = parse(
            "defmodule A do\n  def a <> b, do: a\n  def zero, do: 0\n  def zero do\n    0\n  end\n  def unquote(:dyn)(x), do: x\nend\n",
        );
        let kids = &r.declarations[0].children;
        let names: Vec<&str> = kids.iter().map(|d| d.name.as_str()).collect();
        // Both `zero/0` clauses fold into one entry.
        assert_eq!(names, vec!["<>", "zero", "unquote(:dyn)"]);
        assert_eq!((kids[1].start_line, kids[1].end_line), (3, 6));
    }

    #[test]
    fn dangling_doc_does_not_leak_past_another_macro() {
        let r = parse("defmodule A do\n  @doc \"stray\"\n  plug :x\n  def f, do: 1\nend\n");
        let f = &r.declarations[0].children[0];
        assert!(f.docs.is_empty(), "{:?}", f.docs);
        assert_eq!(f.start_line, 4);
    }
}
