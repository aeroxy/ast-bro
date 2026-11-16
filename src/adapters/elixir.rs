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
//! `@behaviour` lands in the module's `bases` so `implements` can follow it,
//! as does the behaviour a stdlib `use` declares (`use GenServer`).
//!
//! A user macro named `def[a-z_]*` (defk's `defk`, Nx's `defn`) whose first
//! argument is a function head and which has a body defines a function too,
//! with the macro name as its native kind and modifier. That rule runs after
//! every macro the walk knows, so `defstruct`, `defdelegate` and
//! `defoverridable` keep their own meaning.
//!
//! A definition inside another macro's block at module level (`if
//! Code.ensure_loaded?(Jason) do def encode… end`, `for … do def … end`)
//! belongs to the module, so the block is walked as part of the module body,
//! `else` included. A `quote` block is not: its definitions belong to the
//! module the macro expands in.

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
        let mut walker = Walker::new(source);
        // A script (`.exs`) may hold top-level forms outside any module, so
        // the file root is walked with the same body walker as a module.
        let body = walker.walk_body(&root, "");
        let imports = walker
            .directives
            .into_iter()
            .map(|d| ImportBinding {
                local: d.local,
                module: d.module,
                line: d.line,
            })
            .collect();
        ParseResult {
            path: path.to_path_buf(),
            language: self.language_name(),
            source: source.to_vec(),
            line_count: source.iter().filter(|&&b| b == b'\n').count() + 1,
            declarations: body.decls,
            error_count: count_parse_errors(root.clone()),
            imports,
        }
    }
}

/// One `alias` / `import` / `require` / `use`, naming its module in full —
/// or, with keyword `reference`, a module used without one (a remote call
/// or a struct).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Directive {
    pub keyword: &'static str,
    /// The full dotted module name: `__MODULE__` and an earlier alias
    /// in scope are expanded, so `alias Defk.Bench` then `alias Bench.Tick`
    /// names `Defk.Bench.Tick`.
    pub module: String,
    /// The name the file sees: the last segment (or `as:`) for `alias`, the
    /// module itself otherwise.
    pub local: String,
    /// Whether `local` came from `as:`.
    pub renamed: bool,
    pub line: u32,
}

/// What the dependency graph needs from one Elixir file: the modules it
/// defines (nested ones by their full name) and the modules it names.
pub(crate) struct ModuleScan {
    pub defined: Vec<String>,
    pub directives: Vec<Directive>,
}

pub(crate) fn scan_modules(source: &str) -> ModuleScan {
    scan(source, true)
}

/// The modules one Elixir file defines — `scan_modules` without the walk
/// for references, which defining needs none of.
pub(crate) fn defined_modules(source: &str) -> Vec<String> {
    scan(source, false).defined
}

fn scan(source: &str, references: bool) -> ModuleScan {
    use ast_grep_language::{LanguageExt, SupportLang};
    let grep = SupportLang::Elixir.ast_grep(source);
    let mut walker = Walker::new(source.as_bytes());
    walker.scan_references = references;
    walker.walk_body(&grep.root(), "");
    // Directives first, then each referenced module once, so an edge takes
    // the kind of the directive that names it when there is one.
    let (mut directives, references): (Vec<_>, Vec<_>) = walker
        .directives
        .into_iter()
        .partition(|d| d.keyword != "reference");
    let mut seen: std::collections::HashSet<String> =
        directives.iter().map(|d| d.module.clone()).collect();
    directives.extend(
        references
            .into_iter()
            .filter(|r| seen.insert(r.module.clone())),
    );
    ModuleScan {
        defined: walker.defined,
        directives,
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

/// `def`-prefixed macros that never define a function, whatever their
/// arguments look like. The walk handles the ones it understands before the
/// def-like rule is reached; this keeps the rest out of it.
const NOT_DEF_LIKE: &[&str] = &[
    "defmodule",
    "defprotocol",
    "defimpl",
    "defstruct",
    "defexception",
    "defoverridable",
    "defrecord",
    "defrecordp",
];

/// Typespec attributes. A module named in one is not a dependency.
const TYPESPEC_ATTRS: &[&str] = &[
    "spec",
    "type",
    "typep",
    "opaque",
    "callback",
    "macrocallback",
];

/// Modules whose `use` declares the module itself as a behaviour of the
/// user (`@behaviour GenServer`), checked against Elixir 1.20. `use Agent`
/// and `use Task` declare none, and a project module's `__using__` cannot
/// be seen from here.
const USE_BEHAVIOURS: &[&str] = &[
    "GenServer",
    "Supervisor",
    "DynamicSupervisor",
    "Application",
    "Mix.Task",
    "Mix.Task.Compiler",
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
    directives: Vec<Directive>,
    /// Every module defined, by its full name.
    defined: Vec<String>,
    /// Aliases in effect, innermost module last. A module body sees its
    /// parent's aliases and the ones it adds itself, in source order.
    scopes: Vec<std::collections::HashMap<String, String>>,
    /// Also record the modules a form uses without a directive. Only the
    /// dependency graph wants them; `map` skips the walk.
    scan_references: bool,
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
    fn new(src: &'s [u8]) -> Self {
        Walker {
            src,
            directives: Vec::new(),
            defined: Vec::new(),
            scopes: Vec::new(),
            scan_references: false,
        }
    }

    fn walk_body<'a, D: Doc>(&mut self, body: &Node<'a, D>, module_fqn: &str) -> Body {
        let scope = self.scopes.last().cloned().unwrap_or_default();
        self.scopes.push(scope);
        let out = self.walk_forms(body, module_fqn);
        self.scopes.pop();
        out
    }

    fn walk_forms<'a, D: Doc>(&mut self, body: &Node<'a, D>, module_fqn: &str) -> Body {
        let mut out = Body::default();
        let mut pending = Pending::default();
        let mut typedoc: Vec<String> = Vec::new();
        let mut last_fn: Option<LastFn> = None;

        for child in body.children() {
            if !child.is_named() || child.kind() == "comment" {
                continue;
            }
            if self.scan_references {
                let keyword = _call_keyword(&child);
                let walked_elsewhere = matches!(
                    keyword.as_deref(),
                    Some("defmodule" | "defprotocol" | "defimpl")
                        | Some("alias" | "import" | "require" | "use")
                );
                if !walked_elsewhere {
                    // A form's own aliases (`alias` inside a `def`) last
                    // until the form ends.
                    let scope = self.scopes.last().cloned().unwrap_or_default();
                    self.scopes.push(scope);
                    self.references_in(&child, module_fqn);
                    self.scopes.pop();
                }
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
                            let b = _text(v);
                            if !out.behaviours.contains(&b) {
                                out.behaviours.push(b);
                            }
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
                    let decls = self.module_decl(&child, &keyword, module_fqn);
                    // A nested `defmodule B.C` aliases `B` in the
                    // enclosing module.
                    if let Some(decl) = decls.first() {
                        if keyword == "defmodule" && !module_fqn.is_empty() {
                            let first = decl.name.split('.').next().unwrap_or_default();
                            let full = format!("{}.{}", module_fqn, first);
                            self.alias(first.to_string(), full);
                        }
                    }
                    out.decls.extend(decls);
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
                    self.import(&child, &keyword, module_fqn);
                    if keyword == "use" {
                        let used = _named_args(&child)
                            .first()
                            .map(|m| self.expand(&_text(m), module_fqn));
                        if let Some(b) = used.filter(|m| USE_BEHAVIOURS.contains(&m.as_str())) {
                            if !out.behaviours.contains(&b) {
                                out.behaviours.push(b);
                            }
                        }
                    }
                }
                // A user macro that defines a function the way `def` does
                // (`defk`, Nx's `defn`). Last, so it never shadows a macro
                // with handling of its own.
                k if _is_def_like(&child, k) => {
                    let p = std::mem::take(&mut pending);
                    self.function_clause(&child, &keyword, p, &mut out.decls, &mut last_fn);
                }
                // A block holding definitions (`if … do def …`) adds them
                // to this module. Its clauses do not fold with the ones
                // around it, and a `@doc` above it is not theirs.
                k if k != "quote" && !wrapped_bodies(&child).is_empty() => {
                    last_fn = None;
                    pending = Pending::default();
                    for block in wrapped_bodies(&child) {
                        let inner = self.walk_body(&block, module_fqn);
                        out.decls.extend(inner.decls);
                        out.defines_struct |= inner.defines_struct;
                        for b in inner.behaviours {
                            if !out.behaviours.contains(&b) {
                                out.behaviours.push(b);
                            }
                        }
                    }
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
    ) -> Vec<Declaration> {
        let args = _named_args(node);
        let Some(first) = args.first() else {
            return Vec::new();
        };
        let written = _text(first);

        let signature = _module_signature(node, keyword, &written);
        let (kind, names, mut bases, native) = match keyword {
            "defprotocol" => (
                DeclarationKind::Interface,
                vec![(written.clone(), signature)],
                Vec::new(),
                "protocol",
            ),
            "defimpl" => {
                // `defimpl P, for: T` defines `P.T`, and `for: [A, B]` one
                // module per type, each signed with its own `for:`; without
                // `for:` it implements the protocol for the enclosing module.
                let targets = match _keyword_value(&args, "for") {
                    Some(v) if v.kind() == "list" => v
                        .children()
                        .filter(|c| c.is_named() && c.kind() != "comment")
                        .map(|c| {
                            let t = _text(&c);
                            let sig = format!("defimpl {}, for: {}", written, t);
                            (_expand_module(&t, parent_fqn), sig)
                        })
                        .collect(),
                    Some(v) => vec![(_expand_module(&_text(&v), parent_fqn), signature)],
                    None => vec![(parent_fqn.to_string(), signature)],
                };
                let names = targets
                    .into_iter()
                    .map(|(t, sig)| {
                        if t.is_empty() {
                            (written.clone(), sig)
                        } else {
                            (format!("{}.{}", written, t), sig)
                        }
                    })
                    .collect();
                (DeclarationKind::Class, names, vec![written.clone()], "impl")
            }
            _ => (
                DeclarationKind::Class,
                vec![(written.clone(), signature)],
                Vec::new(),
                "module",
            ),
        };

        // `defimpl` names its modules itself (`P.T`); a nested `defmodule`
        // qualifies under its parent.
        let fqn = if keyword == "defimpl" || parent_fqn.is_empty() {
            names
                .first()
                .map(|(n, _)| n.clone())
                .unwrap_or_else(|| written.clone())
        } else {
            format!("{}.{}", parent_fqn, written)
        };
        // A list-form `defimpl` defines one module per type, but its body
        // is walked once, under the first.
        if keyword == "defimpl" {
            self.defined.extend(names.iter().map(|(n, _)| n.clone()));
        } else {
            self.defined.push(fqn.clone());
        }
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
        let decl = Declaration {
            kind,
            name: String::new(),
            signature: String::new(),
            bases,
            attrs: Vec::new(),
            docs: body.moduledoc,
            docs_inside: true,
            group: None,
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
        };
        names
            .into_iter()
            .map(|(name, signature)| Declaration {
                name,
                signature,
                ..decl.clone()
            })
            .collect()
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

        let (private, modifier) = _def_traits(keyword);
        let signature = _function_signature(node, keyword, head);
        let mut modifiers = Vec::new();
        if let Some(m) = modifier {
            modifiers.push(m);
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
            group: None,
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
            group: None,
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
            group: None,
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
            group: None,
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
            group: None,
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
    fn import<'a, D: Doc>(&mut self, node: &Node<'a, D>, keyword: &str, module_fqn: &str) {
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

        let keyword = match keyword {
            "alias" => "alias",
            "import" => "import",
            "require" => "require",
            _ => "use",
        };
        for module in modules {
            let module = self.expand(&module, module_fqn);
            let local = if keyword == "alias" {
                as_name
                    .clone()
                    .unwrap_or_else(|| module.rsplit('.').next().unwrap_or(&module).to_string())
            } else {
                module.clone()
            };
            if keyword == "alias" {
                self.alias(local.clone(), module.clone());
            }
            // `require Foo, as: F` aliases `F` as well.
            if keyword == "require" {
                if let Some(name) = &as_name {
                    self.alias(name.clone(), module.clone());
                }
            }
            self.directives.push(Directive {
                keyword,
                module,
                local,
                renamed: as_name.is_some(),
                line,
            });
        }
    }

    /// Record the modules `node` uses without a directive — every module
    /// name it mentions: a remote call's target (`Defk.Build.build!(…)`), a
    /// struct (`%Defk.Errors{}`), a module passed as a value. That is what
    /// `mix xref` counts as a dependency. Three places are left out because
    /// xref leaves them out too: typespecs, the inside of a `quote` (the
    /// caller's code, not this module's — only its `unquote`s run here), and
    /// a nested module's own name. A directive in the form (`alias` inside a
    /// `def`) is recorded as a directive.
    fn references_in<'a, D: Doc>(&mut self, node: &Node<'a, D>, module_fqn: &str) {
        let line = node.start_pos().line() as u32 + 1;
        match node.kind().as_ref() {
            "alias" => return self.reference(&_text(node), module_fqn, line),
            "dot" if _is_module_dot(node) => return self.reference(&_text(node), module_fqn, line),
            "unary_operator" => {
                if let Some((attr, _)) = _attribute(node) {
                    if TYPESPEC_ATTRS.contains(&attr.as_str()) {
                        return;
                    }
                }
            }
            "call" => match _call_keyword(node).as_deref() {
                Some(kw @ ("alias" | "import" | "require" | "use")) => {
                    let kw = kw.to_string();
                    return self.import(node, &kw, module_fqn);
                }
                Some("quote") => return self.unquoted_in(node, module_fqn),
                Some("defmodule" | "defprotocol" | "defimpl") => {
                    if let Some(b) = node.children().find(|c| c.kind() == "do_block") {
                        self.references_in(&b, module_fqn);
                    }
                    return;
                }
                _ => {}
            },
            _ => {}
        }
        for child in node.children().filter(|c| c.is_named()) {
            self.references_in(&child, module_fqn);
        }
    }

    /// The `unquote(…)` / `unquote_splicing(…)` inside a `quote`: the only
    /// part of it that runs in this module.
    fn unquoted_in<'a, D: Doc>(&mut self, node: &Node<'a, D>, module_fqn: &str) {
        for child in node.children().filter(|c| c.is_named()) {
            let kw = _call_keyword(&child);
            if matches!(kw.as_deref(), Some("unquote" | "unquote_splicing")) {
                for arg in _named_args(&child) {
                    self.references_in(&arg, module_fqn);
                }
            } else {
                self.unquoted_in(&child, module_fqn);
            }
        }
    }

    fn reference(&mut self, written: &str, module_fqn: &str, line: u32) {
        let module = self.expand(written, module_fqn);
        if module.starts_with("__MODULE__") {
            return;
        }
        self.directives.push(Directive {
            keyword: "reference",
            local: module.clone(),
            module,
            renamed: false,
            line,
        });
    }

    /// Bind `local` to `module` for the rest of the current module body.
    fn alias(&mut self, local: String, module: String) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(local, module);
        }
    }

    /// A module reference as Elixir expands it: a leading `__MODULE__` is
    /// the enclosing module, and a leading segment an alias is bound to is
    /// that alias's module. Anything else is already a full name.
    fn expand(&self, module: &str, module_fqn: &str) -> String {
        let (first, rest) = match module.split_once('.') {
            Some((f, r)) => (f, Some(r)),
            None => (module, None),
        };
        if first == "__MODULE__" {
            return _expand_module(module, module_fqn);
        }
        let head = match self.scopes.last().and_then(|s| s.get(first)) {
            Some(full) => full.clone(),
            None => return module.to_string(),
        };
        match rest {
            Some(r) => format!("{}.{}", head, r),
            None => head,
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

/// A definition form as the search chunker sees it: where a chunk may start,
/// what the outline lists, and what a breadcrumb is named after. The adapter
/// walk above builds richer declarations from the same recognition rules.
pub(crate) enum Definition {
    /// `defmodule` / `defprotocol` / `defimpl` — a container of definitions.
    Module { name: String, signature: String },
    /// One clause of a `def`-family definition. Adjacent clauses with the
    /// same keyword, name and arity belong to one function.
    Function {
        keyword: String,
        name: String,
        arity: usize,
        signature: String,
    },
}

/// Recognise `node` as a definition form, by the macro name of its target.
pub(crate) fn definition<'a, D: Doc>(node: &Node<'a, D>) -> Option<Definition> {
    if node.kind() != "call" {
        return None;
    }
    let keyword = _call_keyword(node)?;
    let args = _named_args(node);
    let first = args.first()?;
    match keyword.as_str() {
        "defmodule" | "defprotocol" | "defimpl" => {
            let written = _text(first);
            Some(Definition::Module {
                signature: _module_signature(node, &keyword, &written),
                name: written,
            })
        }
        k if DEF_MACROS.iter().any(|(kw, _, _)| *kw == k) || _is_def_like(node, k) => {
            let (name, arity) = _head_name_arity(first);
            Some(Definition::Function {
                signature: _function_signature(node, &keyword, first),
                keyword,
                name,
                arity,
            })
        }
        _ => None,
    }
}

/// The bodies of a macro call whose `do` or `else` block holds a definition
/// directly (`if … do def … else def … end`), in source order; empty when
/// none does. Such a call wraps definitions rather than being one.
pub(crate) fn wrapped_bodies<'a, D: Doc>(node: &Node<'a, D>) -> Vec<Node<'a, D>> {
    if node.kind() != "call" || definition(node).is_some() {
        return Vec::new();
    }
    let Some(block) = node.children().find(|c| c.kind() == "do_block") else {
        return Vec::new();
    };
    let elses: Vec<_> = block
        .children()
        .filter(|c| c.kind() == "else_block")
        .collect();
    let bodies: Vec<_> = std::iter::once(block).chain(elses).collect();
    if bodies
        .iter()
        .any(|b| b.children().any(|c| definition(&c).is_some()))
    {
        bodies
    } else {
        Vec::new()
    }
}

/// Visibility and modifier of a definition made by `keyword`. A def-like user
/// macro is its own modifier, so digest shows `[defk]`; it is private when it
/// reads as the `p` variant of a one-letter macro (`defnp`, `defkp`) — a
/// longer name ending in `p` (`defstep`) is a word, not a variant.
fn _def_traits(keyword: &str) -> (bool, Option<String>) {
    if let Some((_, private, modifier)) = DEF_MACROS.iter().find(|(kw, _, _)| *kw == keyword) {
        return (*private, modifier.map(str::to_string));
    }
    let private = keyword.len() == 5 && keyword.ends_with('p');
    (private, Some(keyword.to_string()))
}

/// A call to a `def[a-z_]*` macro outside the built-in family that defines
/// a function: its first argument is a function head and it has a body
/// (`do … end` or `do:`). Without both it is some other macro that happens
/// to start with `def` (`defsearch(:name, opts)`, `define(mod, op)`).
fn _is_def_like<'a, D: Doc>(node: &Node<'a, D>, keyword: &str) -> bool {
    let Some(rest) = keyword.strip_prefix("def") else {
        return false;
    };
    if rest.is_empty()
        || !rest.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        || NOT_DEF_LIKE.contains(&keyword)
        || DEF_MACROS.iter().any(|(kw, _, _)| *kw == keyword)
    {
        return false;
    }
    let args = _named_args(node);
    let Some(head) = args.first() else {
        return false;
    };
    let has_body =
        node.children().any(|c| c.kind() == "do_block") || _keyword_value(&args, "do").is_some();
    _is_head(head) && has_body
}

/// `name(args)`, a bare `name`, or either with a guard (`when`) or a return
/// type (`::`) — the heads a def-like macro is written with.
fn _is_head<'a, D: Doc>(node: &Node<'a, D>) -> bool {
    match node.kind().as_ref() {
        "identifier" => true,
        "call" => node
            .field("target")
            .is_some_and(|t| t.kind() == "identifier"),
        "binary_operator" => {
            let op = node.field("operator").map(|o| o.text().into_owned());
            matches!(op.as_deref(), Some("when") | Some("::"))
                && node.field("left").is_some_and(|l| _is_head(&l))
        }
        _ => false,
    }
}

/// `defmodule Foo`, `defprotocol P`, or `defimpl P, for: T` with its whole
/// argument list.
fn _module_signature<'a, D: Doc>(node: &Node<'a, D>, keyword: &str, written: &str) -> String {
    if keyword == "defimpl" {
        let args_text = node
            .children()
            .find(|c| c.kind() == "arguments")
            .map(|a| _text(&a))
            .unwrap_or_else(|| written.to_string());
        return format!("defimpl {}", args_text);
    }
    format!("{} {}", keyword, written)
}

/// The keyword and head of a definition. `defdelegate` keeps its `to:`
/// target; every other macro shows only the head (the `do:` body is not
/// signature).
fn _function_signature<'a, D: Doc>(
    node: &Node<'a, D>,
    keyword: &str,
    head: &Node<'a, D>,
) -> String {
    if keyword == "defdelegate" {
        let args_text = node
            .children()
            .find(|c| c.kind() == "arguments")
            .map(|a| _text(&a))
            .unwrap_or_default();
        return format!("{} {}", keyword, args_text);
    }
    format!("{} {}", keyword, _text(head))
}

/// `__MODULE__.Foo` — a module named relative to the enclosing one.
fn _is_module_dot<'a, D: Doc>(node: &Node<'a, D>) -> bool {
    node.field("left")
        .is_some_and(|l| l.kind() == "identifier" && l.text() == "__MODULE__")
        && node.field("right").is_some_and(|r| r.kind() == "alias")
}

/// `node.text()` with whitespace runs collapsed — multi-line heads and
/// values read as one line.
fn _text<'a, D: Doc>(node: &Node<'a, D>) -> String {
    collapse_ws(&node.text())
}

/// `__MODULE__` or `__MODULE__.Sub` as the module it names inside
/// `enclosing`; any other name, or one outside a module, as written.
fn _expand_module(name: &str, enclosing: &str) -> String {
    match name.strip_prefix("__MODULE__") {
        Some(rest) if !enclosing.is_empty() && (rest.is_empty() || rest.starts_with('.')) => {
            format!("{}{}", enclosing, rest)
        }
        _ => name.to_string(),
    }
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
/// zero-arity `name`, a guarded head (`… when …`), a return-typed head
/// (`f(a :: i64) :: i64`, as defk writes it — `::` itself cannot be
/// defined), operator definitions (`a <> b`, `-a`) and unquoted names
/// (`unquote(n)(args)`).
fn _head_name_arity<'a, D: Doc>(head: &Node<'a, D>) -> (String, usize) {
    match head.kind().as_ref() {
        "binary_operator" => {
            let op = head
                .field("operator")
                .map(|o| o.text().into_owned())
                .unwrap_or_default();
            if op == "when" || op == "::" {
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
    fn stdlib_use_declares_a_behaviour() {
        let r = parse(
            "defmodule A do\n  use GenServer\n  @behaviour GenServer\n  use Agent\n  use Mix.Task\nend\ndefmodule B do\n  alias My.GenServer\n  use GenServer\nend\n",
        );
        let bases: Vec<&[String]> = r.declarations.iter().map(|d| d.bases.as_slice()).collect();
        // Once, however often it is declared; `Agent` declares none; an
        // alias that shadows the stdlib name is the project's module.
        assert_eq!(bases[0], ["GenServer", "Mix.Task"]);
        assert!(bases[1].is_empty(), "{:?}", bases[1]);
    }

    #[test]
    fn definitions_in_a_module_level_block_belong_to_the_module() {
        let r = parse(
            "defmodule A do\n  @doc \"not theirs\"\n  if c() do\n    def f(x), do: x\n  else\n    def f(_), do: nil\n  end\n  unless c(), do: (defp g, do: 1)\n  for n <- [:a] do\n    def unquote(n)(), do: 1\n  end\n  quote do\n    def injected, do: 1\n  end\n  if c() do\n    :no_definitions\n  end\nend\n",
        );
        let a = &r.declarations[0];
        let got: Vec<(&str, usize)> = a
            .children
            .iter()
            .map(|d| (d.signature.as_str(), d.start_line))
            .collect();
        // Both branches, each its own entry; not the keyword-form `unless`
        // (no `do` block), nor the `quote`.
        assert_eq!(
            got,
            vec![("def f(x)", 4), ("def f(_)", 6), ("def unquote(n)()", 10)]
        );
        assert!(a.children[0].docs.is_empty(), "{:?}", a.children[0].docs);
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

    fn scan(src: &str) -> (Vec<String>, Vec<(&'static str, String)>) {
        let s = scan_modules(src);
        let d = s
            .directives
            .into_iter()
            .map(|d| (d.keyword, d.module))
            .collect();
        (s.defined, d)
    }

    #[test]
    fn scan_names_nested_modules_in_full() {
        let (defined, _) = scan(
            "defmodule A do\n  defmodule B.C do\n    defmodule D, do: nil\n  end\nend\ndefimpl P, for: A do\nend\ndefimpl Q, for: [A, B] do\nend\n",
        );
        assert_eq!(defined, vec!["A", "A.B.C", "A.B.C.D", "P.A", "Q.A", "Q.B"]);
    }

    #[test]
    fn scan_expands_aliases_and_module() {
        let (_, d) = scan(
            "defmodule A do\n  alias Foo.Bar\n  alias __MODULE__.{X, Y}\n  import Bar.Baz\n  require Long.Name, as: LN\n  def r, do: LN.x()\n  defmodule N do\n  end\n  def f, do: N.g()\n  def h do\n    alias Deep.Mod\n    Mod.run()\n  end\n  def i, do: Mod.run()\nend\n",
        );
        assert_eq!(
            d,
            vec![
                ("alias", "Foo.Bar".to_string()),
                ("alias", "A.X".to_string()),
                ("alias", "A.Y".to_string()),
                ("import", "Foo.Bar.Baz".to_string()),
                // `require … as:` aliases like `alias … as:`.
                ("require", "Long.Name".to_string()),
                // An alias inside a `def` is a directive, scoped to it.
                ("alias", "Deep.Mod".to_string()),
                // A nested module aliases its short name in the parent.
                ("reference", "A.N".to_string()),
                ("reference", "Mod".to_string()),
            ]
        );
    }

    #[test]
    fn scan_references_skip_typespecs_and_quoted_code() {
        let (_, d) = scan(
            "defmodule A do\n  @spec f(T.t()) :: :ok\n  def f(%S{} = s), do: R.call(s, V)\n  defmacro m do\n    quote do\n      Q.x(unquote(U.y()))\n    end\n  end\n  def e, do: :ets.new(1) && x.y()\nend\n",
        );
        let refs: Vec<&str> = d.iter().map(|(_, m)| m.as_str()).collect();
        assert_eq!(refs, vec!["S", "R", "V", "U"]);
    }

    #[test]
    fn dangling_doc_does_not_leak_past_another_macro() {
        let r = parse("defmodule A do\n  @doc \"stray\"\n  plug :x\n  def f, do: 1\nend\n");
        let f = &r.declarations[0].children[0];
        assert!(f.docs.is_empty(), "{:?}", f.docs);
        assert_eq!(f.start_line, 4);
    }

    #[test]
    fn defimpl_targets_expand_module_and_lists() {
        let r = parse(
            "defimpl P, for: [] do\nend\ndefmodule A do\n  defimpl P, for: [__MODULE__.B, C] do\n  end\n  defimpl P, for: __MODULE__ do\n  end\nend\ndefimpl P, for: __MODULE__ do\nend\n",
        );
        let top: Vec<&str> = r.declarations.iter().map(|d| d.name.as_str()).collect();
        // `for: []` defines no module; outside a module `__MODULE__` stays as written.
        assert_eq!(top, vec!["A", "P.__MODULE__"]);
        let kids: Vec<&str> = r.declarations[0]
            .children
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(kids, vec!["P.A.B", "P.C", "P.A"]);
    }
}
