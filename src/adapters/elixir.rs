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
//! A def-like user macro (Nx's `defn`) with a function head and a body is a
//! function, its macro name the native kind.
//!
//! Definitions in a module-level `if` / `for` block belong to the module; a
//! `quote`'s do not.
//!
//! ExUnit's `test` / `setup` are functions named as ExUnit compiles them
//! (`test group inner`), and `describe` is a `Namespace`.
//!
//! Each function carries the calls in its body, receivers expanded to full
//! module names.

use super::base::{collapse_ws, count_parse_errors, LanguageAdapter};
use crate::core::{
  CallKind, CallSite, Declaration, DeclarationKind, ImportBinding, ImportOnly, Imported,
  ParseResult, ReceiverScope,
};
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

/// An `alias` / `import` / `require` / `use`, a `defimpl`'s protocol
/// (`impl`), or (`reference`) a module named without a directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Directive {
  pub keyword: &'static str,
  /// The full module name, with `__MODULE__` and aliases in scope expanded.
  pub module: String,
  /// The name the file sees: for `alias`, the last segment or `as:`.
  pub local: String,
  /// Whether `local` came from `as:`.
  pub renamed: bool,
  pub line: u32,
}

/// The modules one file defines and the modules it names.
pub(crate) struct ModuleScan {
  pub defined: Vec<String>,
  pub directives: Vec<Directive>,
}

pub(crate) fn scan_modules(source: &str) -> ModuleScan {
  scan(source)
}

/// The argument counts a definition accepts, `(min, max)`, read from its
/// signature: `def f(a, b \\ 1)` accepts one or two, and `@callback f(a)
/// :: t` declares one. `None` without a head.
pub(crate) fn arity_range(signature: &str) -> Option<(usize, usize)> {
  use ast_grep_language::{LanguageExt, SupportLang};
  let grep = SupportLang::Elixir.ast_grep(signature);
  let top = grep.root().children().find(|c| c.is_named())?;
  if top.kind() == "unary_operator" {
    let spec = _named_args(&top.field("operand")?).into_iter().next()?;
    let (_, arity) = _head_name_arity(&_spec_head(&spec));
    return Some((arity, arity));
  }
  let def = grep.root().children().find(|c| c.kind() == "call")?;
  let keyword = _call_keyword(&def).unwrap_or_default();
  let (head, _) = _def_head(_named_args(&def).into_iter().next()?, &keyword);
  let (_, arity) = _head_name_arity(&head);
  let defaults = match head.kind().as_ref() {
    "call" => _named_args(&head)
      .iter()
      .filter(|p| {
        p.kind() == "binary_operator" && p.field("operator").is_some_and(|o| o.text() == "\\\\")
      })
      .count(),
    _ => 0,
  };
  Some((arity - defaults, arity))
}

/// The modules one file defines. A module defined in a function body or an
/// `if` block is reached only by the walk for references.
pub(crate) fn defined_modules(source: &str) -> Vec<String> {
  scan(source).defined
}

fn scan(source: &str) -> ModuleScan {
  use ast_grep_language::{LanguageExt, SupportLang};
  let grep = SupportLang::Elixir.ast_grep(source);
  let mut walker = Walker::new(source.as_bytes());
  walker.scan_references = true;
  walker.walk_body(&grep.root(), "");
  // Directives first, so an edge takes the kind of its directive.
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

/// `def`-prefixed macros that never define a function.
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

/// Stdlib modules whose `use` declares them as a behaviour (Elixir 1.20).
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
const ANNOTATION_ATTRS: &[&str] = &["spec", "impl", "deprecated", "tag"];

/// Macros that are syntax, not call sites; the code inside them is walked.
const NOT_CALLS: &[&str] = &[
  "if",
  "unless",
  "case",
  "cond",
  "with",
  "for",
  "try",
  "receive",
  "fn",
  "super",
  "unquote",
  "unquote_splicing",
  "import",
  "require",
  "use",
  "def",
  "defp",
  "defmacro",
  "defmacrop",
  "defguard",
  "defguardp",
  "defdelegate",
  "defmodule",
  "defprotocol",
  "defimpl",
  "defstruct",
  "defexception",
  "defoverridable",
  "__block__",
];

/// Longest attribute value / type body kept verbatim in a signature; longer
/// ones are elided so a multi-line map literal cannot flood a `map` line.
const MAX_INLINE_VALUE: usize = 60;
const MAX_INLINE_TYPE: usize = 100;

struct Walker<'s> {
  src: &'s [u8],
  directives: Vec<Directive>,
  /// Every module defined, by its full name.
  defined: Vec<String>,
  /// Aliases and imports in effect, innermost scope last.
  scopes: Vec<Scope>,
  /// Also record modules named without a directive (deps only).
  scan_references: bool,
  /// The `describe` label ExUnit puts in the name of the tests inside it.
  describe: Option<String>,
  /// How many wrappers (`if … do def …`) the walk is inside. It enters at
  /// most `MAX_WRAPPER_DEPTH`, as many as `wrapped_bodies` looks through,
  /// so a chain where every level defines cannot exhaust the stack.
  wrappers: usize,
}

/// A lexical scope: a module body, a function, or a block.
#[derive(Clone, Default)]
struct Scope {
  aliases: std::collections::HashMap<String, String>,
  imports: Vec<Import>,
}

/// An `import`, with its `only:` and `except:` selectors.
#[derive(Clone)]
struct Import {
  module: String,
  /// `None` imports every name.
  only: Option<Vec<(String, usize)>>,
  except: Vec<(String, usize)>,
  /// `only: :functions` or `only: :macros`, which the resolver applies.
  kinds: ImportOnly,
}

impl Import {
  fn admits(&self, name: &str, arity: Option<usize>) -> bool {
    let listed = |list: &[(String, usize)]| {
      list
        .iter()
        .any(|(n, a)| n == name && arity.is_none_or(|x| x == *a))
    };
    self.only.as_deref().is_none_or(listed) && !listed(&self.except)
  }
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
      describe: None,
      wrappers: 0,
    }
  }

  fn walk_body<'a, D: Doc>(&mut self, body: &Node<'a, D>, module_fqn: &str) -> Body {
    self.walk_scoped(body.children().collect(), module_fqn)
  }

  /// Walk `forms` as one body, in a scope of their own.
  fn walk_scoped<'a, D: Doc>(&mut self, forms: Vec<Node<'a, D>>, module_fqn: &str) -> Body {
    let scope = self.scopes.last().cloned().unwrap_or_default();
    self.scopes.push(scope);
    let out = self.walk_forms(forms, module_fqn);
    self.scopes.pop();
    out
  }

  fn walk_forms<'a, D: Doc>(&mut self, forms: Vec<Node<'a, D>>, module_fqn: &str) -> Body {
    let mut out = Body::default();
    let mut pending = Pending::default();
    let mut typedoc: Vec<String> = Vec::new();
    let mut last_fn: Option<LastFn> = None;

    for child in forms {
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
          // An `alias` inside a `def` ends with it.
          let scope = self.scopes.last().cloned().unwrap_or_default();
          self.scopes.push(scope);
          // The blocks of `if … do def … end` are walked as part of
          // the module body below; only the condition is left. A
          // test's body is its own, even when it defines a module.
          if !matches!(
            keyword.as_deref(),
            Some("quote" | "test" | "setup" | "setup_all")
          ) && !wrapped_bodies(&child).is_empty()
          {
            for arg in _named_args(&child) {
              self.references_in(&arg, module_fqn);
            }
          } else {
            self.references_in(&child, module_fqn);
          }
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
              let b = self.expand(&_text(v), module_fqn);
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
            out
              .decls
              .extend(self.type_decl(&child, &attr, value.as_ref(), docs));
          }
          _ => {
            last_fn = None;
            out
              .decls
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
          self.nested_alias(&child, &keyword, module_fqn);
          out
            .decls
            .extend(self.module_decl(&child, &keyword, module_fqn));
        }
        k if DEF_MACROS.iter().any(|(kw, _, _)| *kw == k) => {
          let p = std::mem::take(&mut pending);
          self.function_clause(
            &child,
            &keyword,
            p,
            module_fqn,
            &mut out.decls,
            &mut last_fn,
          );
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
        // A def-like user macro (Nx's `defn`); last, so it shadows none.
        k if _is_def_like(&child, k) => {
          let p = std::mem::take(&mut pending);
          self.function_clause(
            &child,
            &keyword,
            p,
            module_fqn,
            &mut out.decls,
            &mut last_fn,
          );
        }
        k @ ("test" | "describe" | "setup" | "setup_all") if _is_exunit_block(&child, k) => {
          // A clause after the test still folds into those before
          // it, as across any other macro call.
          let p = std::mem::take(&mut pending);
          self.exunit_decl(&child, k, p, module_fqn, &mut out);
        }
        // `if … do def … end` defines into this module. A clause
        // in the block joins the function's clauses around it, and
        // a clause after it still folds into those before.
        k if k != "quote"
          && self.wrappers < MAX_WRAPPER_DEPTH
          && !wrapped_bodies(&child).is_empty() =>
        {
          pending = Pending::default();
          for block in wrapped_bodies(&child) {
            self.wrappers += 1;
            let inner = self.walk_body(&block, module_fqn);
            self.wrappers -= 1;
            for d in inner.decls {
              _fold_clause(&mut out.decls, d);
            }
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
        let line = node.start_pos().line() as u32 + 1;
        let targets: Vec<(String, String)> = match _keyword_value(&args, "for") {
          Some(v) if v.kind() == "list" => v
            .children()
            .filter(|c| c.is_named() && c.kind() != "comment")
            .map(|c| {
              let t = _text(&c);
              let sig = format!("defimpl {}, for: {}", written, t);
              (self.expand(&t, parent_fqn), sig)
            })
            .collect(),
          Some(v) => vec![(self.expand(&_text(&v), parent_fqn), signature)],
          None => vec![(parent_fqn.to_string(), signature)],
        };
        let protocol = self.expand(&written, parent_fqn);
        // The protocol is a dependency like `import` (`mix xref`
        // labels both export); each type named in `for:` is a reference.
        if self.scan_references {
          self.directives.push(Directive {
            keyword: "impl",
            module: protocol.clone(),
            local: protocol.clone(),
            renamed: false,
            line,
          });
          if _keyword_value(&args, "for").is_some() {
            for (t, _) in &targets {
              self.reference_expanded(t.clone(), line);
            }
          }
        }
        let names = targets
          .into_iter()
          .map(|(t, sig)| {
            if t.is_empty() {
              (protocol.clone(), sig)
            } else {
              (format!("{}.{}", protocol, t), sig)
            }
          })
          .collect();
        (DeclarationKind::Class, names, vec![protocol], "impl")
      }
      _ => (
        DeclarationKind::Class,
        vec![(written.clone(), signature)],
        Vec::new(),
        "module",
      ),
    };

    // `defimpl` names its modules (`P.T`); a nested `defmodule` qualifies.
    let fqn = if keyword == "defimpl" || parent_fqn.is_empty() {
      names
        .first()
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| written.clone())
    } else if written.starts_with("__MODULE__") {
      _expand_module(&written, parent_fqn)
    } else if let Some(absolute) = written.strip_prefix("Elixir.") {
      absolute.to_string()
    } else {
      format!("{}.{}", parent_fqn, written)
    };
    // A list-form `defimpl`'s body is walked once, under the first type.
    if keyword == "defimpl" {
      self.defined.extend(names.iter().map(|(n, _)| n.clone()));
    } else {
      self.defined.push(fqn.clone());
    }
    let body = self.walk_scoped(module_forms(node), &fqn);
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
    module_fqn: &str,
    out: &mut Vec<Declaration>,
    last_fn: &mut Option<LastFn>,
  ) {
    let args = _named_args(node);
    let Some(head) = args.first() else {
      return;
    };
    let (name, arity) = _head_name_arity(&_def_head(head.clone(), keyword).0);
    let range = node.range();
    let end_line = node.end_pos().line() + 1;
    let calls = self.clause_calls(node, keyword, &args, module_fqn);

    let continues = last_fn
      .as_ref()
      .is_some_and(|l| l.keyword == keyword && l.name == name && l.arity == arity);
    if continues {
      // The entry need not be the last: a module-level block's
      // definitions may sit between two clauses.
      if let Some(prev) = out
        .iter_mut()
        .rev()
        .find(|d| _same_function(d, keyword, &name, arity))
      {
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
        prev.calls.extend(calls);
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
      calls,
    });
  }

  /// ExUnit's `test` / `setup` / `setup_all` as a function, `describe` as a
  /// namespace.
  fn exunit_decl<'a, D: Doc>(
    &mut self,
    node: &Node<'a, D>,
    keyword: &str,
    pending: Pending,
    module_fqn: &str,
    out: &mut Body,
  ) {
    let args = _named_args(node);
    let label = _exunit_label(&args);
    let signature = _exunit_signature(&args, keyword);
    let calls = if keyword == "describe" {
      Vec::new()
    } else {
      self.body_calls(node, module_fqn)
    };
    // A `describe` adds no module level: a module defined in it belongs to
    // the enclosing module.
    let mut modules = Vec::new();
    let (kind, name, children) = match keyword {
      "describe" => {
        let mut children = Vec::new();
        if let Some(block) = node.children().find(|c| c.kind() == "do_block") {
          let outer = self.describe.replace(label.clone());
          let inner = self.walk_body(&block, module_fqn);
          self.describe = outer;
          (modules, children) = inner.decls.into_iter().partition(|d| {
            matches!(
              d.native_kind.as_deref(),
              Some("module" | "protocol" | "impl")
            )
          });
          out.defines_struct |= inner.defines_struct;
          for b in inner.behaviours {
            if !out.behaviours.contains(&b) {
              out.behaviours.push(b);
            }
          }
        }
        (
          DeclarationKind::Namespace,
          format!("describe {}", label),
          children,
        )
      }
      "test" => {
        let name = match &self.describe {
          Some(group) => format!("test {} {}", group, label),
          None => format!("test {}", label),
        };
        (DeclarationKind::Function, name, Vec::new())
      }
      _ => (DeclarationKind::Function, keyword.to_string(), Vec::new()),
    };
    // A `@tag` belongs to the test below it, not to a `describe`.
    let pending = if keyword == "test" {
      pending
    } else {
      Pending::default()
    };
    let range = node.range();
    let (start_byte, start_line) = pending
      .start
      .unwrap_or((range.start, node.start_pos().line() + 1));
    out.decls.push(Declaration {
      kind,
      name,
      signature,
      bases: Vec::new(),
      attrs: pending.attrs,
      docs: Vec::new(),
      docs_inside: false,
      group: None,
      visibility: String::new(),
      start_line,
      end_line: node.end_pos().line() + 1,
      start_byte,
      end_byte: range.end,
      doc_start_byte: start_byte,
      native_kind: Some(keyword.to_string()),
      modifiers: Vec::new(),
      deprecated: false,
      children,
      calls,
    });
    out.decls.extend(modules);
  }

  /// The calls in a clause's guard and body; a `defdelegate` calls `to:`.
  fn clause_calls<'a, D: Doc>(
    &mut self,
    node: &Node<'a, D>,
    keyword: &str,
    args: &[Node<'a, D>],
    module_fqn: &str,
  ) -> Vec<CallSite> {
    let mut out = Vec::new();
    if self.scan_references {
      return out;
    }
    if keyword == "defdelegate" {
      if let Some(head) = args.first() {
        self.default_calls(head, module_fqn, &mut out);
      }
      if let (Some(head), Some(to)) = (args.first(), _keyword_value(args, "to")) {
        // The target gets every argument of the head, defaults too.
        let (head_name, arity) = _head_name_arity(head);
        let name = match _keyword_value(args, "as") {
          Some(a) => _text(&a).trim_start_matches(':').to_string(),
          None => head_name,
        };
        out.push(CallSite {
          name,
          receiver: Some(self.module_name(&to, module_fqn)),
          line: node.start_pos().line() as u32 + 1,
          kind: CallKind::Call,
          receiver_scope: ReceiverScope::Absolute,
          imports: Vec::new(),
          arity: Some(arity),
        });
      }
      return out;
    }
    let scope = self.scopes.last().cloned().unwrap_or_default();
    self.scopes.push(scope);
    if let Some(head) = args.first() {
      let (call, guard) = _def_head(head.clone(), keyword);
      if let Some(guard) = guard {
        self.calls_in(&guard, module_fqn, &mut out);
      }
      self.default_calls(&call, module_fqn, &mut out);
    }
    for arg in args.iter().skip(1) {
      self.calls_in(arg, module_fqn, &mut out);
    }
    if let Some(block) = node.children().find(|c| c.kind() == "do_block") {
      self.calls_in(&block, module_fqn, &mut out);
    }
    self.scopes.pop();
    out
  }

  /// The calls in a head's default arguments (`x \\ compute()`), which
  /// are evaluated at the call.
  fn default_calls<'a, D: Doc>(
    &mut self,
    head: &Node<'a, D>,
    module_fqn: &str,
    out: &mut Vec<CallSite>,
  ) {
    for param in _named_args(head) {
      if param.kind() == "binary_operator"
        && param.field("operator").is_some_and(|o| o.text() == "\\\\")
      {
        if let Some(default) = param.field("right") {
          self.calls_in(&default, module_fqn, out);
        }
      }
    }
  }

  /// The calls in a `test` / `setup` body.
  fn body_calls<'a, D: Doc>(&mut self, node: &Node<'a, D>, module_fqn: &str) -> Vec<CallSite> {
    let mut out = Vec::new();
    if self.scan_references {
      return out;
    }
    let scope = self.scopes.last().cloned().unwrap_or_default();
    self.scopes.push(scope);
    if let Some(body) = _keyword_value(&_named_args(node), "do") {
      self.calls_in(&body, module_fqn, &mut out);
    }
    if let Some(block) = node.children().find(|c| c.kind() == "do_block") {
      self.calls_in(&block, module_fqn, &mut out);
    }
    self.scopes.pop();
    out
  }

  /// Record the call sites in `node`. The caller scopes a body's aliases
  /// and imports; a block inside it scopes its own.
  /// The walk keeps its own stack: a long operator chain (`a <> b <> …`)
  /// nests as deep as it is long.
  fn calls_in<'a, D: Doc>(
    &mut self,
    node: &Node<'a, D>,
    module_fqn: &str,
    out: &mut Vec<CallSite>,
  ) {
    enum Step<N> {
      /// A node, and the arguments a `|>` passes to it.
      Visit(N, usize),
      /// A `|>` whose left side is walked: its target is next.
      Pipe(N),
      /// The end of a block's scope.
      Leave,
    }
    let mut stack = vec![Step::Visit(node.clone(), 0)];
    while let Some(step) = stack.pop() {
      let (node, piped) = match step {
        Step::Visit(node, piped) => (node, piped),
        Step::Pipe(node) => {
          if let Some(right) = node.field("right") {
            let line = right.start_pos().line() as u32 + 1;
            out.push(self.site(_text(&right), None, line, CallKind::Call, Some(1)));
          }
          continue;
        }
        Step::Leave => {
          self.scopes.pop();
          continue;
        }
      };
      let line = node.start_pos().line() as u32 + 1;
      match node.kind().as_ref() {
        "call" => match node.field("target") {
          Some(t) if t.kind() == "identifier" => match _text(&t).as_str() {
            "quote" => {
              // `bind_quoted:` values are evaluated where the quote is.
              let options = _named_args(&node);
              if let Some(bound) = _keyword_value(&options, "bind_quoted") {
                self.calls_in(&bound, module_fqn, out);
              }
              self.unquoted_calls(&node, module_fqn, out);
              continue;
            }
            keyword @ ("alias" | "import" | "require") => {
              // Scoped here, but not one of the module's imports.
              let recorded = self.directives.len();
              self.import(&node, keyword, module_fqn);
              self.directives.truncate(recorded);
              continue;
            }
            name
              if NOT_CALLS.contains(&name)
                && !self.kernel_excludes(name, _call_arity(&node, piped)) => {}
            name => {
              let arity = _call_arity(&node, piped);
              out.push(self.site(name.to_string(), None, line, CallKind::Call, Some(arity)));
            }
          },
          // `fun.(x)` has no `right`; `map.key` reads a field.
          Some(t) if t.kind() == "dot" => {
            if let (Some(left), Some(right)) = (t.field("left"), t.field("right")) {
              let receiver = self.module_name(&left, module_fqn);
              let parenthesised = node.children().any(|c| c.kind() == "arguments");
              let module = receiver.starts_with(|c: char| c.is_ascii_uppercase() || c == ':');
              // `&Mod.fun/2` writes its arity after the `/`.
              let captured = node
                .parent()
                .filter(|p| p.field("operator").is_some_and(|o| o.text() == "/"))
                .filter(|p| {
                  p.parent()
                    .and_then(|c| c.field("operator"))
                    .is_some_and(|o| o.text() == "&")
                });
              let arity = match captured {
                Some(p) if !parenthesised => p.field("right").and_then(|r| r.text().parse().ok()),
                _ => Some(_call_arity(&node, piped)),
              };
              if parenthesised || module {
                let site = self.site(_text(&right), Some(receiver), line, CallKind::Call, arity);
                out.push(site);
              }
            }
          }
          _ => {}
        },
        // `x |> helper` calls `helper` without parentheses, and
        // `x |> helper(y)` passes `helper` one more argument.
        "binary_operator" if node.field("operator").is_some_and(|o| o.text() == "|>") => {
          match node.field("right") {
            Some(r) if r.kind() == "identifier" => {
              let left = node.field("left");
              stack.push(Step::Pipe(node));
              if let Some(left) = left {
                stack.push(Step::Visit(left, 0));
              }
              continue;
            }
            Some(r) if r.kind() == "call" => {
              let left = node.field("left");
              stack.push(Step::Visit(r, 1));
              if let Some(left) = left {
                stack.push(Step::Visit(left, 0));
              }
              continue;
            }
            _ => {}
          }
        }
        // A pattern (`%Mod{} = x`, `{:ok, x} <- f()`) calls nothing; its
        // guard (`{:ok, x} when ok(x) <- f()`) does.
        "binary_operator"
          if node
            .field("operator")
            .is_some_and(|o| matches!(o.text().as_ref(), "=" | "<-")) =>
        {
          let guard = node
            .field("left")
            .filter(|l| l.field("operator").is_some_and(|o| o.text() == "when"))
            .and_then(|l| l.field("right"));
          if let Some(guard) = guard {
            stack.push(Step::Visit(guard, 0));
          }
          if let Some(right) = node.field("right") {
            stack.push(Step::Visit(right, 0));
          }
          continue;
        }
        // `&helper/1`; `&Mod.fun/2` is a call node and handled above.
        "unary_operator" if node.field("operator").is_some_and(|o| o.text() == "&") => {
          let capture = node
            .field("operand")
            .filter(|o| o.kind() == "binary_operator");
          let local = capture
            .as_ref()
            .and_then(|o| o.field("left"))
            .filter(|l| l.kind() == "identifier");
          if let Some(name) = local {
            let arity = capture
              .and_then(|o| o.field("right"))
              .and_then(|r| r.text().parse().ok());
            out.push(self.site(_text(&name), None, line, CallKind::Call, arity));
            continue;
          }
        }
        // `%Mod{…}` builds the struct `Mod` defines.
        "map" => {
          if let Some(s) = node.children().find(|c| c.kind() == "struct") {
            if let Some(m) = s.children().find(|c| c.is_named()) {
              let module = self.module_name(&m, module_fqn);
              out.push(self.site(module, None, line, CallKind::Construct, None));
            }
          }
        }
        // An `alias` or `import` in a block ends with it.
        "do_block" | "else_block" | "after_block" | "rescue_block" | "catch_block"
        | "stab_clause" => {
          let scope = self.scopes.last().cloned().unwrap_or_default();
          self.scopes.push(scope);
          stack.push(Step::Leave);
          // A clause's left side is patterns and a guard, except
          // in `cond`, where it is a condition, and in a `receive`
          // timeout (`after timeout() -> …`), where it is a value.
          let block = node.parent();
          let in_cond = block
            .as_ref()
            .and_then(|block| block.parent())
            .is_some_and(|call| _call_keyword(&call).as_deref() == Some("cond"));
          let timeout = block.is_some_and(|block| block.kind() == "after_block");
          if node.kind() == "stab_clause" && !in_cond && !timeout {
            if let Some(body) = node.field("right") {
              stack.push(Step::Visit(body, 0));
            }
            let guard = node
              .field("left")
              .filter(|l| l.kind() == "binary_operator")
              .and_then(|l| l.field("right"));
            if let Some(guard) = guard {
              stack.push(Step::Visit(guard, 0));
            }
            continue;
          }
        }
        _ => {}
      }
      let start = stack.len();
      stack.extend(
        node
          .children()
          .filter(|c| c.is_named())
          .map(|c| Step::Visit(c, 0)),
      );
      stack[start..].reverse();
    }
  }

  /// A call site. An expanded alias or an Erlang module names a module in
  /// full; a bare call may name a function of a module imported here.
  fn site(
    &self,
    name: String,
    receiver: Option<String>,
    line: u32,
    kind: CallKind,
    arity: Option<usize>,
  ) -> CallSite {
    let receiver_scope = match receiver.as_deref() {
      Some(r) if r.starts_with(|c: char| c.is_ascii_uppercase() || c == ':') => {
        ReceiverScope::Absolute
      }
      _ => ReceiverScope::None,
    };
    let imports = match (&receiver, kind) {
      (None, CallKind::Call) => self
        .scopes
        .last()
        .into_iter()
        .flat_map(|s| &s.imports)
        .filter(|i| i.admits(&name, arity))
        .map(|i| Imported {
          module: i.module.clone(),
          only: i.kinds,
        })
        .collect(),
      _ => Vec::new(),
    };
    CallSite {
      name,
      receiver,
      line,
      kind,
      receiver_scope,
      imports,
      arity,
    }
  }

  /// Whether `import Kernel, except: [if: 2]` is in effect here, which
  /// leaves `if/2` to another import, as Elixir allows no two.
  fn kernel_excludes(&self, name: &str, arity: usize) -> bool {
    self.scopes.last().is_some_and(|s| {
      s.imports
        .iter()
        .any(|i| i.module == "Kernel" && !i.admits(name, Some(arity)))
    })
  }

  /// The calls inside a `quote`'s `unquote(…)`s.
  fn unquoted_calls<'a, D: Doc>(
    &mut self,
    node: &Node<'a, D>,
    module_fqn: &str,
    out: &mut Vec<CallSite>,
  ) {
    let mut stack = Vec::new();
    _push_children(&mut stack, node);
    while let Some(child) = stack.pop() {
      let kw = _call_keyword(&child);
      if matches!(kw.as_deref(), Some("unquote" | "unquote_splicing")) {
        for arg in _named_args(&child) {
          self.calls_in(&arg, module_fqn, out);
        }
      } else {
        _push_children(&mut stack, &child);
      }
    }
  }

  /// A call's receiver, expanded when it is an alias or `__MODULE__`.
  fn module_name<'a, D: Doc>(&self, node: &Node<'a, D>, module_fqn: &str) -> String {
    let text = _text(node);
    if node.kind() == "alias" || text == "__MODULE__" || text.starts_with("__MODULE__.") {
      self.expand(&text, module_fqn)
    } else {
      text
    }
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
      // `as:` names the module for `alias` and `require`; for `use`
      // it is an option passed on to `__using__`.
      let renamed = as_name.is_some() && keyword != "use";
      let local = match &as_name {
        Some(name) if renamed => name.clone(),
        _ if keyword == "alias" => module.rsplit('.').next().unwrap_or(&module).to_string(),
        _ => module.clone(),
      };
      if keyword == "alias" {
        self.alias(local.clone(), module.clone());
      }
      if keyword == "import" {
        let only_arg = _keyword_value(&args, "only");
        let (only, kinds) = match &only_arg {
          Some(v) => _selector(v),
          None => (None, ImportOnly::Any),
        };
        let except = _keyword_value(&args, "except")
          .and_then(|v| _selector(&v).0)
          .unwrap_or_default();
        if let Some(scope) = self.scopes.last_mut() {
          // A module imported again replaces its earlier import,
          // except that `except:` alone filters it, per kind: a
          // kind it did not import comes in minus `except:`.
          let (earlier, rest): (Vec<Import>, Vec<Import>) = std::mem::take(&mut scope.imports)
            .into_iter()
            .partition(|i| i.module == module);
          scope.imports = rest;
          if only_arg.is_none() && !except.is_empty() && !earlier.is_empty() {
            let has = |k| {
              earlier
                .iter()
                .any(|i| i.kinds == ImportOnly::Any || i.kinds == k)
            };
            let missing = match (has(ImportOnly::Functions), has(ImportOnly::Macros)) {
              (true, false) => Some(ImportOnly::Macros),
              (false, true) => Some(ImportOnly::Functions),
              _ => None,
            };
            for mut i in earlier {
              i.except.extend(except.iter().cloned());
              scope.imports.push(i);
            }
            if let Some(kinds) = missing {
              scope.imports.push(Import {
                module: module.clone(),
                only: None,
                except,
                kinds,
              });
            }
          } else {
            scope.imports.push(Import {
              module: module.clone(),
              only,
              except,
              kinds,
            });
          }
        }
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
        renamed,
        line,
      });
    }
    // Modules in the options (`use Ext, transformers: [Mod]`) are
    // dependencies too; an `as:` name is not.
    if self.scan_references {
      let pairs = args
        .iter()
        .skip(1)
        .filter(|a| a.kind() == "keywords")
        .flat_map(|k| k.children().filter(|c| c.kind() == "pair"));
      for pair in pairs {
        let key = pair.field("key").map(|k| _text(&k)).unwrap_or_default();
        if key.trim().trim_end_matches(':') != "as" {
          if let Some(value) = pair.field("value") {
            self.references_in(&value, module_fqn);
          }
        }
      }
    }
  }

  /// Record the modules `node` names without a directive, as `mix xref`
  /// counts them: typespecs and `quote` bodies (but their `unquote`s) aside.
  ///
  /// The walk keeps its own stack: a long operator chain (`a <> b <> …`)
  /// nests as deep as it is long, and recursion would overflow the thread.
  fn references_in<'a, D: Doc>(&mut self, node: &Node<'a, D>, module_fqn: &str) {
    let mut stack = vec![node.clone()];
    while let Some(node) = stack.pop() {
      let line = node.start_pos().line() as u32 + 1;
      match node.kind().as_ref() {
        "alias" => {
          self.reference(&_text(&node), module_fqn, line);
          continue;
        }
        "dot" if _is_module_dot(&node) => {
          self.reference(&_text(&node), module_fqn, line);
          continue;
        }
        "unary_operator" => {
          if let Some((attr, _)) = _attribute(&node) {
            if TYPESPEC_ATTRS.contains(&attr.as_str()) {
              continue;
            }
          }
        }
        "call" => match _call_keyword(&node).as_deref() {
          Some(kw @ ("alias" | "import" | "require" | "use")) => {
            let kw = kw.to_string();
            self.import(&node, &kw, module_fqn);
            continue;
          }
          Some("quote") => {
            self.unquoted_in(&node, module_fqn);
            continue;
          }
          // A module defined in a function body or an `if` block.
          Some(kw @ ("defmodule" | "defprotocol" | "defimpl")) => {
            let kw = kw.to_string();
            self.nested_alias(&node, &kw, module_fqn);
            self.module_decl(&node, &kw, module_fqn);
            continue;
          }
          Some(kw) if DEF_MACROS.iter().any(|(k, _, _)| *k == kw) => {
            self.definition_references(&node, module_fqn);
            continue;
          }
          _ => {}
        },
        _ => {}
      }
      _push_children(&mut stack, &node);
    }
  }

  /// A definition's head is a pattern, not a call: `def import(%M{} = m)`
  /// names `M` and runs no `import`.
  fn definition_references<'a, D: Doc>(&mut self, node: &Node<'a, D>, module_fqn: &str) {
    for child in node.children().filter(|c| c.is_named()) {
      if child.kind() != "arguments" {
        self.references_in(&child, module_fqn);
        continue;
      }
      let mut args = child.children().filter(|c| c.is_named());
      let Some(mut head) = args.next() else {
        continue;
      };
      let mut guard = None;
      if head.kind() == "binary_operator"
        && head.field("operator").is_some_and(|o| o.text() == "when")
      {
        guard = head.field("right");
        if let Some(left) = head.field("left") {
          head = left;
        }
      }
      if head.kind() == "call" {
        for arg in _named_args(&head) {
          self.references_in(&arg, module_fqn);
        }
      } else {
        self.references_in(&head, module_fqn);
      }
      if let Some(guard) = guard {
        self.references_in(&guard, module_fqn);
      }
      for rest in args {
        self.references_in(&rest, module_fqn);
      }
    }
  }

  /// The `unquote(…)`s inside a `quote`, the part that runs here.
  fn unquoted_in<'a, D: Doc>(&mut self, node: &Node<'a, D>, module_fqn: &str) {
    let mut stack = Vec::new();
    _push_children(&mut stack, node);
    while let Some(child) = stack.pop() {
      let kw = _call_keyword(&child);
      if matches!(kw.as_deref(), Some("unquote" | "unquote_splicing")) {
        for arg in _named_args(&child) {
          self.references_in(&arg, module_fqn);
        }
      } else {
        _push_children(&mut stack, &child);
      }
    }
  }

  fn reference(&mut self, written: &str, module_fqn: &str, line: u32) {
    let module = self.expand(written, module_fqn);
    self.reference_expanded(module, line);
  }

  fn reference_expanded(&mut self, module: String, line: u32) {
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

  /// A nested `defmodule B.C` aliases `B` in its parent, before its own
  /// body is read; `defmodule __MODULE__.C` and `Elixir.C` alias nothing.
  fn nested_alias<'a, D: Doc>(&mut self, node: &Node<'a, D>, keyword: &str, module_fqn: &str) {
    if keyword != "defmodule" || module_fqn.is_empty() {
      return;
    }
    let Some(written) = _named_args(node).first().map(|n| _text(n)) else {
      return;
    };
    if written.starts_with("__MODULE__") || written.starts_with("Elixir.") {
      return;
    }
    let first = written.split('.').next().unwrap_or_default();
    self.alias(first.to_string(), format!("{}.{}", module_fqn, first));
  }

  /// Bind `local` to `module` for the rest of the current module body.
  fn alias(&mut self, local: String, module: String) {
    if let Some(scope) = self.scopes.last_mut() {
      scope.aliases.insert(local, module);
    }
  }

  /// A module reference with a leading `__MODULE__` or alias expanded.
  fn expand(&self, module: &str, module_fqn: &str) -> String {
    let (first, rest) = match module.split_once('.') {
      Some((f, r)) => (f, Some(r)),
      None => (module, None),
    };
    if first == "__MODULE__" {
      return _expand_module(module, module_fqn);
    }
    // `Elixir.Foo` is `Foo`, whatever `Foo` is aliased to.
    if first == "Elixir" {
      if let Some(r) = rest {
        return r.to_string();
      }
    }
    let head = match self.scopes.last().and_then(|s| s.aliases.get(first)) {
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
    text
      .lines()
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

/// Push `node`'s named children so they pop in source order.
fn _push_children<'a, D: Doc>(stack: &mut Vec<Node<'a, D>>, node: &Node<'a, D>) {
  let start = stack.len();
  stack.extend(node.children().filter(|c| c.is_named()));
  stack[start..].reverse();
}

/// The forms of a module's body: the children of its `do` block, or its
/// `do:` value, a block of forms (`do: (def a …; def b …)`) or one form
/// (`do: def(a, …)`).
pub(crate) fn module_forms<'a, D: Doc>(node: &Node<'a, D>) -> Vec<Node<'a, D>> {
  if let Some(block) = node.children().find(|c| c.kind() == "do_block") {
    return block.children().collect();
  }
  match _keyword_value(&_named_args(node), "do") {
    Some(v) if v.kind() == "block" => v.children().collect(),
    Some(v) => vec![v],
    None => Vec::new(),
  }
}

/// A definition form, as the search chunker sees it.
pub(crate) enum Definition {
  /// `defmodule` / `defprotocol` / `defimpl`, with the full name its body
  /// is in (`fqn`), which aliases aside is the one `map` gives it.
  Module {
    name: String,
    fqn: String,
    signature: String,
  },
  /// One clause of a `def`-family definition.
  Function {
    keyword: String,
    name: String,
    arity: usize,
    signature: String,
  },
}

/// Recognise `node` as a definition form, by the macro name of its target.
/// `enclosing` is the full name of the module `node` is in, `""` at the top.
pub(crate) fn definition<'a, D: Doc>(node: &Node<'a, D>, enclosing: &str) -> Option<Definition> {
  if node.kind() != "call" {
    return None;
  }
  let keyword = _call_keyword(node)?;
  let args = _named_args(node);
  let first = args.first()?;
  match keyword.as_str() {
    "defmodule" | "defprotocol" | "defimpl" => {
      let written = _text(first);
      // `defimpl P, for: T` defines `P.T`; a list form's body is walked
      // under its first type, as in `map`.
      let target = _keyword_value(&args, "for")
        .filter(|_| keyword == "defimpl")
        .and_then(|v| match v.kind().as_ref() {
          "list" => v
            .children()
            .find(|c| c.is_named() && c.kind() != "comment")
            .map(|c| _text(&c)),
          _ => Some(_text(&v)),
        });
      let signature = _module_signature(node, &keyword, &written);
      // As the module walk names it, aliases aside: `defimpl` names its
      // module in full, a nested `defmodule` qualifies.
      let (name, fqn) = match target {
        Some(t) => {
          let name = format!("{}.{}", written, _expand_module(&t, enclosing));
          (name.clone(), name)
        }
        // With no `for:`, a `defimpl` is for the module it is in.
        None if keyword == "defimpl" && !enclosing.is_empty() => {
          let name = format!("{}.{}", written, enclosing);
          (name.clone(), name)
        }
        None if keyword == "defimpl" || enclosing.is_empty() => (written.clone(), written),
        None if written.starts_with("__MODULE__") => {
          let fqn = _expand_module(&written, enclosing);
          (written, fqn)
        }
        None => match written.strip_prefix("Elixir.") {
          Some(absolute) => (written.clone(), absolute.to_string()),
          None => {
            let fqn = format!("{}.{}", enclosing, written);
            (written, fqn)
          }
        },
      };
      Some(Definition::Module {
        name,
        fqn,
        signature,
      })
    }
    k if DEF_MACROS.iter().any(|(kw, _, _)| *kw == k) || _is_def_like(node, k) => {
      let (name, arity) = _head_name_arity(&_def_head(first.clone(), &keyword).0);
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

/// ExUnit's `test "…"` / `describe "…"` / `setup` / `setup_all` with a body.
fn _is_exunit_block<'a, D: Doc>(node: &Node<'a, D>, keyword: &str) -> bool {
  let args = _named_args(node);
  let has_body =
    node.children().any(|c| c.kind() == "do_block") || _keyword_value(&args, "do").is_some();
  has_body
    && match keyword {
      "test" | "describe" => args.first().is_some_and(|a| a.kind() == "string"),
      _ => true,
    }
}

/// The header of an ExUnit block: its keyword and arguments, without a
/// `do:` body.
fn _exunit_signature<'a, D: Doc>(args: &[Node<'a, D>], keyword: &str) -> String {
  let shown: Vec<String> = args
    .iter()
    .filter(|a| a.kind() != "keywords")
    .map(|a| _text(a))
    .collect();
  if shown.is_empty() {
    keyword.to_string()
  } else {
    format!("{} {}", keyword, shown.join(", "))
  }
}

/// The label of an ExUnit block as its name writes it. A qn joins its
/// segments with `::`, so a label writes it `: :` (`test "Foo::bar"` is no
/// function `bar`); the signature keeps it.
fn _exunit_label<'a, D: Doc>(args: &[Node<'a, D>]) -> String {
  _exunit_text(args).replace("::", ": :")
}

/// The label of an ExUnit block as written, for a breadcrumb, which is no qn.
fn _exunit_text<'a, D: Doc>(args: &[Node<'a, D>]) -> String {
  args
    .first()
    .filter(|a| a.kind() == "string")
    .map(|a| _string_content(&_text(a)))
    .unwrap_or_default()
}

/// An ExUnit block, as the search chunker sees it.
pub(crate) enum ExUnitBlock {
  /// A `describe`, with its label as written (a breadcrumb is no qn) and
  /// its signature as `map` writes it.
  Describe { name: String, signature: String },
  /// A `test` / `setup` / `setup_all`: one member, whatever its body holds.
  Leaf { signature: String },
}

/// Recognise `node` as an ExUnit block, as the module walk does.
pub(crate) fn exunit_block<'a, D: Doc>(node: &Node<'a, D>) -> Option<ExUnitBlock> {
  if node.kind() != "call" {
    return None;
  }
  let keyword = _call_keyword(node)?;
  let args = _named_args(node);
  let signature = _exunit_signature(&args, &keyword);
  match keyword.as_str() {
    "describe" if _is_exunit_block(node, &keyword) => Some(ExUnitBlock::Describe {
      name: format!("describe {}", _exunit_text(&args)),
      signature,
    }),
    "test" | "setup" | "setup_all" if _is_exunit_block(node, &keyword) => {
      Some(ExUnitBlock::Leaf { signature })
    }
    _ => None,
  }
}

/// A string literal's text without its delimiters.
fn _string_content(literal: &str) -> String {
  let inner = literal
    .strip_prefix("\"\"\"")
    .and_then(|s| s.strip_suffix("\"\"\""))
    .or_else(|| literal.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
    .unwrap_or(literal);
  inner.trim().to_string()
}

/// How many wrappers deep `wrapped_bodies` looks for a definition. Unbounded,
/// the check recursed once per level, so a few hundred nested `if`s
/// exhausted the stack and the time grew with the square of the depth.
const MAX_WRAPPER_DEPTH: usize = 16;

/// The `do` and `else` blocks of a call that wraps definitions (`if … do def
/// …`); empty when it wraps none.
pub(crate) fn wrapped_bodies<'a, D: Doc>(node: &Node<'a, D>) -> Vec<Node<'a, D>> {
  wrapped_bodies_within(node, MAX_WRAPPER_DEPTH)
}

/// `wrapped_bodies`, looking through at most `depth` more nested wrappers.
fn wrapped_bodies_within<'a, D: Doc>(node: &Node<'a, D>, depth: usize) -> Vec<Node<'a, D>> {
  if node.kind() != "call" || definition(node, "").is_some() {
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
  // An ExUnit `test` / `describe` / `setup` defines into the module too, and
  // so does a wrapper inside this one (`if … do if … do def …`), but not a
  // `quote`, whose code is data.
  let defines = |c: &Node<'a, D>| {
    definition(c, "").is_some()
      || _call_keyword(c).is_some_and(|k| match k.as_str() {
        "test" | "describe" | "setup" | "setup_all" => _is_exunit_block(c, &k),
        "quote" => false,
        _ => depth > 0 && !wrapped_bodies_within(c, depth - 1).is_empty(),
      })
  };
  if bodies.iter().any(|b| b.children().any(|c| defines(&c))) {
    bodies
  } else {
    Vec::new()
  }
}

/// Whether `d` is the entry for `keyword name/arity`.
fn _same_function(d: &Declaration, keyword: &str, name: &str, arity: usize) -> bool {
  d.kind == DeclarationKind::Function
    && d.native_kind.as_deref() == Some(keyword)
    && d.name == name
    && arity_range(&d.signature).map(|(_, max)| max) == Some(arity)
}

/// Add `d` to `decls`, folded into the entry of the same function if one is
/// there: one function is one entry, whichever block holds its clauses.
fn _fold_clause(decls: &mut Vec<Declaration>, d: Declaration) {
  let arity = arity_range(&d.signature).map(|(_, max)| max);
  let keyword = d.native_kind.clone().unwrap_or_default();
  let prev = arity.and_then(|a| {
    decls
      .iter_mut()
      .find(|p| _same_function(p, &keyword, &d.name, a))
  });
  let Some(prev) = prev else {
    decls.push(d);
    return;
  };
  if d.start_byte < prev.start_byte {
    prev.start_line = d.start_line;
    prev.start_byte = d.start_byte;
    prev.doc_start_byte = d.doc_start_byte;
  }
  if d.end_byte > prev.end_byte {
    prev.end_line = d.end_line;
    prev.end_byte = d.end_byte;
  }
  for a in d.attrs {
    if !prev.attrs.contains(&a) {
      prev.attrs.push(a);
    }
  }
  if prev.docs.is_empty() {
    prev.docs = d.docs;
  }
  prev.deprecated |= d.deprecated;
  prev.calls.extend(d.calls);
}

/// Visibility and modifier for `keyword`. A def-like macro is its own modifier,
/// private as the `p` of a one-letter macro (`defnp`).
fn _def_traits(keyword: &str) -> (bool, Option<String>) {
  if let Some((_, private, modifier)) = DEF_MACROS.iter().find(|(kw, _, _)| *kw == keyword) {
    return (*private, modifier.map(str::to_string));
  }
  let private = keyword.len() == 5 && keyword.ends_with('p');
  (private, Some(keyword.to_string()))
}

/// A `def[a-z_]*` user macro with a function head and a body; without both it
/// is some other macro (`define(mod, op)`).
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

/// A function head: `name(args)` or `name`, with an optional guard or `::`.
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
    // The arguments up to a `do:` pair, which is the body, as a `do` block
    // is in the other form.
    let body = _keyword_value(&_named_args(node), "do")
      .and_then(|v| v.parent())
      .map(|pair| pair.range().start);
    let args_text = node
      .children()
      .find(|c| c.kind() == "arguments")
      .map(|a| {
        let text = a.text();
        let end = body.map_or(text.len(), |b| b.saturating_sub(a.range().start));
        collapse_ws(text.get(..end).unwrap_or(&text))
          .trim_end_matches(|c: char| c == ',' || c.is_whitespace())
          .to_string()
      })
      .unwrap_or_else(|| written.to_string());
    return format!("defimpl {}", args_text);
  }
  format!("{} {}", keyword, written)
}

/// The keyword and head of a definition; `defdelegate` keeps its `to:`.
fn _function_signature<'a, D: Doc>(
  node: &Node<'a, D>,
  keyword: &str,
  head: &Node<'a, D>,
) -> String {
  if keyword == "defdelegate" {
    let args_text = node
      .children()
      .find(|c| c.kind() == "arguments")
      .map(|a| _code_text(&a))
      .unwrap_or_default();
    return format!("{} {}", keyword, args_text);
  }
  format!("{} {}", keyword, _code_text(head))
}

/// `__MODULE__.Foo` — a module named relative to the enclosing one.
fn _is_module_dot<'a, D: Doc>(node: &Node<'a, D>) -> bool {
  node
    .field("left")
    .is_some_and(|l| l.kind() == "identifier" && l.text() == "__MODULE__")
    && node.field("right").is_some_and(|r| r.kind() == "alias")
}

/// `node.text()` with whitespace runs collapsed — multi-line heads and
/// values read as one line.
fn _text<'a, D: Doc>(node: &Node<'a, D>) -> String {
  collapse_ws(&node.text())
}

/// `_text` without the `#` comments in `node`: once the lines are joined, a
/// comment would hide the rest of the head (`opts \\ []`).
fn _code_text<'a, D: Doc>(node: &Node<'a, D>) -> String {
  let start = node.range().start;
  let text = node.text();
  let mut comments = Vec::new();
  let mut stack = vec![node.clone()];
  while let Some(n) = stack.pop() {
    if n.kind() == "comment" {
      comments.push(n.range());
    } else {
      stack.extend(n.children());
    }
  }
  comments.sort_by_key(|r| r.start);
  let mut code = String::new();
  let mut at = 0;
  for r in comments {
    code.push_str(&text[at..r.start - start]);
    at = r.end - start;
  }
  code.push_str(&text[at..]);
  collapse_ws(&code)
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

/// The argument count of a call: its arguments, a `do` block, and the
/// value a `|>` passes it.
fn _call_arity<'a, D: Doc>(node: &Node<'a, D>, piped: usize) -> usize {
  let block = node.children().any(|c| c.kind() == "do_block");
  _named_args(node).len() + usize::from(block) + piped
}

/// Named, non-comment children of a call's `arguments` node.
fn _named_args<'a, D: Doc>(node: &Node<'a, D>) -> Vec<Node<'a, D>> {
  node
    .children()
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

/// The names an `import` selector lists, `[fmt: 1]` or `[{:fmt, 1}]`, and
/// the kinds it selects. `:functions` and `:macros` select every name of
/// one kind, which the resolver tells apart; `:sigils` selects no name a
/// call uses.
fn _selector<'a, D: Doc>(value: &Node<'a, D>) -> (Option<Vec<(String, usize)>>, ImportOnly) {
  match value.text().as_ref() {
    ":functions" => return (None, ImportOnly::Functions),
    ":macros" => return (None, ImportOnly::Macros),
    _ if value.kind() == "atom" => return (Some(Vec::new()), ImportOnly::Any),
    _ => {}
  }
  let pairs = value
    .children()
    .filter(|c| c.kind() == "keywords")
    .flat_map(|k| {
      k.children()
        .filter(|c| c.kind() == "pair")
        .collect::<Vec<_>>()
    })
    .filter_map(|pair| {
      let name = pair
        .field("key")?
        .text()
        .trim()
        .trim_end_matches(':')
        .to_string();
      Some((name, pair.field("value")?.text().parse().ok()?))
    });
  let tuples = value
    .children()
    .filter(|c| c.kind() == "tuple")
    .filter_map(|t| {
      let mut parts = t.children().filter(|c| c.is_named());
      let name = parts.next()?.text().trim_start_matches(':').to_string();
      Some((name, parts.next()?.text().parse().ok()?))
    });
  (Some(pairs.chain(tuples).collect()), ImportOnly::Any)
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
/// (`f(a :: integer) :: integer`, as typed DSLs write it), operator
/// definitions (`a <> b`, `-a`) and unquoted names
/// (`unquote(n)(args)`).
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

/// The call a definition's head names, and its guard: `f(x) when g`. A user
/// macro's head may also carry a return type (`deftyped f(x) :: t when g`),
/// but `defmacro left :: right` defines the operator `::` itself.
fn _def_head<'a, D: Doc>(head: Node<'a, D>, keyword: &str) -> (Node<'a, D>, Option<Node<'a, D>>) {
  let op = |n: &Node<'a, D>, want: &str| {
    n.kind() == "binary_operator" && n.field("operator").is_some_and(|o| o.text() == want)
  };
  let (mut head, mut guard) = (head, None);
  if op(&head, "when") {
    guard = head.field("right");
    head = head.field("left").unwrap_or(head);
  }
  if op(&head, "::") && !DEF_MACROS.iter().any(|(kw, _, _)| *kw == keyword) {
    head = head.field("left").unwrap_or(head);
  }
  (head, guard)
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
    // Both branches, one function so one entry; not the keyword-form
    // `unless` (no `do` block), nor the `quote`.
    assert_eq!(got, vec![("def f(x)", 4), ("def unquote(n)()", 10)]);
    assert_eq!(a.children[0].end_line, 6);
    assert!(a.children[0].docs.is_empty(), "{:?}", a.children[0].docs);
  }

  #[test]
  fn a_keyword_bodied_implementation_is_signed_without_its_body() {
    // As the `do … end` form is: the `do:` pair is the body.
    let r = parse("defimpl P, for: A, do: (def a(_), do: 1)\ndefimpl P, for: B do\nend\n");
    let sigs: Vec<&str> = r
      .declarations
      .iter()
      .map(|d| d.signature.as_str())
      .collect();
    assert_eq!(sigs, vec!["defimpl P, for: A", "defimpl P, for: B"]);
  }

  #[test]
  fn a_keyword_bodied_module_defines_its_functions() {
    // `do:` is the same body as `do … end`, one form or a block of them.
    let r = parse(
      "defmodule M, do: (def run(), do: helper(); defp helper, do: :ok)\ndefmodule N, do: def(go(x), do: x)\n",
    );
    let names = |i: usize| -> Vec<&str> {
      r.declarations[i]
        .children
        .iter()
        .map(|d| d.name.as_str())
        .collect()
    };
    assert_eq!(names(0), vec!["run", "helper"]);
    assert_eq!(names(1), vec!["go"]);
    // The calls in it are the function's, as in a `do` block.
    let run = &r.declarations[0].children[0];
    assert!(
      run.calls.iter().any(|c| c.name == "helper"),
      "{:?}",
      run.calls
    );
  }

  #[test]
  fn definitions_in_nested_blocks_belong_to_the_module() {
    let r = parse(
      "defmodule A do\n  if a() do\n    if b() do\n      def f, do: 1\n    end\n  end\n  if a() do\n    quote do\n      def injected, do: 1\n    end\n  end\nend\n",
    );
    let got: Vec<&str> = r.declarations[0]
      .children
      .iter()
      .map(|d| d.signature.as_str())
      .collect();
    // An `if` holding only an `if` still wraps a definition; a `quote` does not.
    assert_eq!(got, vec!["def f"]);
  }

  #[test]
  fn a_deep_stack_of_blocks_does_not_exhaust_the_stack() {
    // The nested-wrapper check looks a bounded depth down. Unbounded, it
    // recursed once per level and aborted on a few hundred nested `if`s.
    for (depth, inner) in [(600, "def f, do: 1\n"), (1500, "")] {
      let src = format!(
        "defmodule A do\n{}{}{}end\n",
        "if a() do\n".repeat(depth),
        inner,
        "end\n".repeat(depth)
      );
      let r = parse(&src);
      assert_eq!(r.declarations[0].name, "A");
    }
    // Every level defines, so every level is a wrapper: the walk enters
    // as many of them as the check looks through, and no more.
    let depth = 3000;
    let levels: String = (0..depth)
      .map(|i| format!("if a() do\ndef f{i}, do: 1\n"))
      .collect();
    let src = format!("defmodule A do\n{levels}{}end\n", "end\n".repeat(depth));
    let r = parse(&src);
    let names: Vec<&str> = r.declarations[0]
      .children
      .iter()
      .map(|d| d.name.as_str())
      .collect();
    assert_eq!(names.len(), MAX_WRAPPER_DEPTH, "{names:?}");
    assert_eq!(names[0], "f0");
  }

  #[test]
  fn clauses_fold_across_a_test_between_them() {
    // Kernel.WarningTest: `purge/1` has a clause on each side of tests;
    // main folds them into one entry, at the first clause.
    let r = parse(
            "defmodule W do\n  defp purge(l) when is_list(l), do: l\n  test \"t\" do\n    :ok\n  end\n  defp purge(m) when is_atom(m), do: m\nend\n",
        );
    let got: Vec<(&str, usize, usize)> = r.declarations[0]
      .children
      .iter()
      .map(|d| (d.name.as_str(), d.start_line, d.end_line))
      .collect();
    assert_eq!(got, vec![("purge", 2, 6), ("test t", 3, 5)]);
  }

  #[test]
  fn tests_inside_a_module_level_block_belong_to_the_module() {
    let r = parse(
            "defmodule T do\n  if c() do\n    test \"works\" do\n      Target.run()\n    end\n  end\n  for n <- [1, 2] do\n    describe \"group\" do\n      test \"each\", do: n\n    end\n  end\nend\n",
        );
    let got: Vec<&str> = r.declarations[0]
      .children
      .iter()
      .map(|d| d.name.as_str())
      .collect();
    assert_eq!(got, vec!["test works", "describe group"]);
    let calls: Vec<&str> = r.declarations[0].children[0]
      .calls
      .iter()
      .map(|c| c.name.as_str())
      .collect();
    assert_eq!(calls, vec!["run"]);
  }

  #[test]
  fn a_module_in_a_describe_belongs_to_the_module() {
    // A `describe` adds no module level: Elixir defines `D.Helper`.
    let r = parse(
            "defmodule D do\n  describe \"group\" do\n    defmodule Helper do\n      def go, do: :ok\n    end\n\n    test \"uses it\" do\n      Helper.go()\n    end\n  end\nend\n",
        );
    let names = |ds: &[Declaration]| -> Vec<String> { ds.iter().map(|d| d.name.clone()).collect() };
    let d = &r.declarations[0];
    assert_eq!(names(&d.children), vec!["describe group", "Helper"]);
    assert_eq!(names(&d.children[0].children), vec!["test group uses it"]);
    // A protocol and an implementation are modules too.
    let r = parse(
      "defmodule D do\n  describe \"group\" do\n    defprotocol Proto do\n      def go(x)\n    end\n\n    defimpl Proto, for: Atom do\n      def go(_), do: :ok\n    end\n  end\nend\n",
    );
    let d = &r.declarations[0];
    assert_eq!(
      names(&d.children),
      vec!["describe group", "Proto", "Proto.Atom"]
    );
    assert!(d.children[0].children.is_empty());
  }

  #[test]
  fn only_a_user_def_macro_reads_through_a_typed_head() {
    // Kernel.SpecialForms defines the operator `::`; `deftyped` is a user
    // macro whose head carries a return type.
    let r = parse(
            "defmodule K do\n  defmacro left :: right do\n    {left, right}\n  end\n  deftyped f(x \\\\ 1) :: integer do\n    x\n  end\n  deftyped g(x \\\\ D.v()) :: t, do: x\nend\n",
        );
    let got: Vec<(&str, Option<(usize, usize)>)> = r.declarations[0]
      .children
      .iter()
      .map(|d| (d.name.as_str(), arity_range(&d.signature)))
      .collect();
    assert_eq!(
      got,
      vec![
        ("::", Some((2, 2))),
        ("f", Some((0, 1))),
        ("g", Some((0, 1)))
      ]
    );
    // A default argument's call is the function's.
    let g = &r.declarations[0].children[2];
    assert_eq!(
      g.calls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
      ["v"]
    );
  }

  #[test]
  fn clauses_around_a_module_level_block_fold_into_one_entry() {
    // Plug.Conn.Status: `code/1` has clauses before, inside and after a
    // `for`. One function is one entry, at its first clause, as on main.
    let r = parse(
            "defmodule S do\n  def code(i) when is_integer(i), do: i\n  for {c, a} <- [ok: 200] do\n    def code(unquote(a)), do: unquote(c)\n    def other(unquote(a)), do: 1\n  end\n  def code(x), do: raise(x)\n  def code(x, y), do: {x, y}\nend\n",
        );
    let got: Vec<(&str, usize, usize)> = r.declarations[0]
      .children
      .iter()
      .map(|d| (d.signature.as_str(), d.start_line, d.end_line))
      .collect();
    assert_eq!(
      got,
      vec![
        ("def code(i) when is_integer(i)", 2, 7),
        ("def other(unquote(a))", 5, 5),
        ("def code(x, y)", 8, 8),
      ]
    );
    let calls: Vec<&str> = r.declarations[0].children[0]
      .calls
      .iter()
      .map(|c| c.name.as_str())
      .collect();
    assert_eq!(calls, vec!["is_integer", "raise"]);
  }

  #[test]
  fn exunit_blocks_are_named_as_exunit_compiles_them() {
    let r = parse(
            "defmodule T do\n  setup do\n    :ok\n  end\n  setup :named\n  @tag :slow\n  test \"plain\", %{n: n} do\n    n\n  end\n  test \"kw\", do: :ok\n  test \"pending\"\n  describe \"group\" do\n    @tag :x\n    test \"inner\" do\n      :ok\n    end\n    defp helper, do: 1\n  end\nend\n",
        );
    let t = &r.declarations[0];
    let got: Vec<(&str, &str, DeclarationKind)> = t
      .children
      .iter()
      .map(|d| (d.name.as_str(), d.signature.as_str(), d.kind))
      .collect();
    use DeclarationKind::*;
    assert_eq!(
      got,
      vec![
        ("setup", "setup", Function),
        ("test plain", "test \"plain\", %{n: n}", Function),
        ("test kw", "test \"kw\"", Function),
        ("describe group", "describe \"group\"", Namespace),
      ]
    );
    assert_eq!(t.children[1].attrs, ["@tag :slow"]);
    assert_eq!(t.children[1].start_line, 6, "range widens to its @tag");
    let inner: Vec<&str> = t.children[3]
      .children
      .iter()
      .map(|d| d.name.as_str())
      .collect();
    assert_eq!(inner, ["test group inner", "helper"]);
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
            "defmodule A do\n  alias Foo.Bar\n  alias __MODULE__.{X, Y}\n  import Bar.Baz\n  require Long.Name, as: LN\n  def r, do: LN.x()\n  use Ext, steps: [Step.One], as: NotAModule\n  defmodule N do\n  end\n  def f, do: N.g()\n  def h do\n    alias Deep.Mod\n    Mod.run()\n  end\n  def i, do: Mod.run()\nend\n",
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
        ("use", "Ext".to_string()),
        // An alias inside a `def` is a directive, scoped to it.
        ("alias", "Deep.Mod".to_string()),
        // A nested module aliases its short name in the parent.
        // A module in a directive's options is a dependency.
        ("reference", "Step.One".to_string()),
        ("reference", "A.N".to_string()),
        ("reference", "Mod".to_string()),
      ]
    );
  }

  #[test]
  fn scan_names_modules_as_elixir_does() {
    let src = "if Code.ensure_loaded?(J) do\n  defmodule Cond do\n  end\nend\ndefmodule A do\n  defmodule Inner do\n    def new, do: %Inner{}\n  end\n  defmodule __MODULE__.Sub do\n  end\n  def t do\n    defmodule Fwd do\n    end\n    Fwd.x()\n  end\n  def u, do: Sub.y()\nend\n";
    let (defined, d) = scan(src);
    assert_eq!(defined, vec!["Cond", "A", "A.Inner", "A.Sub", "A.Fwd"]);
    let refs: Vec<&str> = d.iter().map(|(_, m)| m.as_str()).collect();
    // `Inner` is aliased before its body is read; `__MODULE__.Sub`
    // aliases nothing.
    assert_eq!(refs, vec!["Code", "J", "A.Inner", "A.Fwd", "Sub"]);
  }

  #[test]
  fn scan_expands_module_references_and_keeps_body_aliases_in_the_body() {
    let (_, d) = scan(
            "defmodule A do\n  def f, do: __MODULE__.X.g()\n  def t do\n    defmodule Fwd do\n    end\n    Fwd.x()\n  end\n  def u, do: Fwd.y()\nend\n",
        );
    let refs: Vec<&str> = d.iter().map(|(_, m)| m.as_str()).collect();
    // `Fwd` is aliased inside `t` only.
    assert_eq!(refs, vec!["A.X", "A.Fwd", "Fwd"]);
  }

  #[test]
  fn scan_aliases_end_with_their_module() {
    let (_, d) = scan(
            "defmodule A do\n  alias X.Foo\n  def f, do: Foo.y()\nend\ndefmodule B do\n  def g, do: Foo.y()\nend\n",
        );
    let refs: Vec<&str> = d.iter().map(|(_, m)| m.as_str()).collect();
    assert_eq!(refs, vec!["X.Foo", "Foo"]);
  }

  #[test]
  fn scan_reads_a_definition_head_as_a_pattern() {
    let (_, d) = scan(
            "defmodule A do\n  def import(%S{} = s), do: s\n  def require(x) when is_struct(x, G), do: x\nend\n",
        );
    assert_eq!(
      d,
      vec![
        ("reference", "S".to_string()),
        ("reference", "G".to_string())
      ]
    );
  }

  #[test]
  fn scan_survives_a_long_operator_chain() {
    let chain = vec!["\"a\""; 20_000].join(" <> ");
    let src = format!("defmodule A do\n  def s, do: {chain} <> M.x()\nend\n");
    let (_, d) = scan(&src);
    assert_eq!(d, vec![("reference", "M".to_string())]);
  }

  #[test]
  fn scan_reads_as_and_the_elixir_prefix() {
    let src = "defmodule A do\n  alias X.Y\n  require R, as: Q\n  use U, as: V\n  def f, do: Elixir.Y.z()\n  defmodule Elixir.Top do\n  end\nend\n";
    let s = scan_modules(src);
    let d: Vec<(&str, &str, bool)> = s
      .directives
      .iter()
      .map(|d| (d.module.as_str(), d.local.as_str(), d.renamed))
      .collect();
    // `as:` renames for `require`, not for `use`; `Elixir.Y` is `Y`,
    // not the alias `X.Y`.
    assert_eq!(
      d,
      vec![
        ("X.Y", "Y", false),
        ("R", "Q", true),
        ("U", "U", false),
        ("Y", "Y", false),
      ]
    );
    assert_eq!(s.defined, vec!["A", "Top"]);
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
  fn scan_references_read_a_test_body_that_defines_a_module() {
    // A test is not a module-level wrapper: its body runs in the test,
    // so a `defmodule` inside it does not hide the test's other calls.
    let (defined, d) = scan(
            "defmodule BazTest do\n  test \"runs a mock\" do\n    defmodule MyMock do\n      def call(x), do: Foo.bar(x)\n    end\n    assert Baz.run(MyMock) == 1\n  end\n  setup do\n    defmodule M2 do\n    end\n    Qux.go()\n  end\nend\n",
        );
    let refs: Vec<&str> = d.iter().map(|(_, m)| m.as_str()).collect();
    assert_eq!(refs, vec!["Foo", "Baz", "BazTest.MyMock", "Qux"]);
    assert_eq!(defined, vec!["BazTest", "BazTest.MyMock", "BazTest.M2"]);
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
