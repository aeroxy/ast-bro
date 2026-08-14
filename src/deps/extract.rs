//! Per-language import extraction. Wraps existing `surface::imports`
//! extractors for Rust/Python/TS/JS/Scala and adds new tree-sitter
//! passes for Java/C#/Kotlin/Go.
//!
//! All extractors emit `RawImport` records — language-agnostic units
//! the resolver consumes. Each record carries enough info that the
//! renderer can show the original statement + line.

use ast_grep_core::{Doc, Node};
use ast_grep_language::{LanguageExt, SupportLang};
use std::path::Path;

use crate::adapters::base::collapse_ws;
use crate::deps::graph::ImportKind;
use crate::deps::resolver::build::Lang;
use crate::surface::imports as surface_imports;

/// One extracted import. Targets are normalised slash-joined module
/// paths (e.g. `com/foo/Bar`, `crate/net/client`); resolver does the
/// final mapping to a file.
#[derive(Debug, Clone)]
pub struct RawImport {
    pub spec: String,
    pub kind: ImportKind,
    pub line: u32,
    /// Display-only: the original statement source line.
    #[allow(dead_code)]
    pub statement: String,
    pub local_name: Option<String>,
    /// Source dotted path (preserves dots, no slashes).
    pub raw_path: Option<String>,
}

/// Top-level dispatch — extract every import from `path`. Returns
/// nothing for unrecognised extensions. Caller is responsible for
/// reading the file (we do it here).
pub fn extract(path: &Path, lang: Lang) -> Vec<RawImport> {
    let Ok(src) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    match lang {
        Lang::Rust => extract_rust(&src),
        Lang::Python => extract_python(&src),
        Lang::TypeScript | Lang::Tsx | Lang::JavaScript => extract_typescript(&src, lang),
        Lang::Scala => extract_scala(&src),
        Lang::Java => extract_java(&src),
        Lang::Kotlin => extract_kotlin(&src),
        Lang::CSharp => extract_csharp(&src),
        Lang::Go => extract_go(&src),
        Lang::Cpp => extract_cpp(&src),
        Lang::Php => extract_php(&src),
        Lang::Ruby => extract_ruby(&src),
        Lang::Other => Vec::new(),
    }
}

// ---- Rust ----

fn extract_rust(src: &str) -> Vec<RawImport> {
    let imports = surface_imports::extract_rust_imports(src);
    let mut out = Vec::new();
    for u in imports.uses {
        let kind = match u.kind {
            surface_imports::UseSegmentKind::Item => ImportKind::Use,
            surface_imports::UseSegmentKind::Glob => ImportKind::Glob,
        };
        out.push(RawImport {
            spec: u.path.clone(),
            kind,
            line: u.line as u32,
            statement: u.statement,
            local_name: u.alias,
            raw_path: Some(u.path),
        });
    }
    for m in imports.mods {
        if !m.is_external_file {
            continue;
        }
        out.push(RawImport {
            spec: format!("self::{}", m.name),
            kind: ImportKind::Mod,
            line: m.line as u32,
            statement: format!("mod {};", m.name),
            local_name: None,
            raw_path: Some(m.name),
        });
    }
    out
}

// ---- Python ----

fn extract_python(src: &str) -> Vec<RawImport> {
    let mut out = Vec::new();
    let imports = surface_imports::extract_python_imports(src);
    for fi in imports.from_imports {
        let prefix: String = ".".repeat(fi.relative_dots);
        let module = if fi.module.is_empty() {
            prefix.clone()
        } else {
            format!("{}{}", prefix, fi.module)
        };
        if fi.is_glob {
            // Treat `from x import *` as one edge to the source module.
            let spec = normalise_python(&module, fi.relative_dots);
            out.push(RawImport {
                spec,
                kind: ImportKind::StarFrom,
                line: fi.line as u32,
                statement: fi.statement.clone(),
                local_name: None,
                raw_path: Some(module.clone()),
            });
            continue;
        }
        // For each name imported, emit one edge — first try resolving
        // the full `module.name` path (e.g. `pkg.sub.fn`), the resolver
        // will fall back to dropping the trailing segment.
        if fi.names.is_empty() {
            let spec = normalise_python(&module, fi.relative_dots);
            out.push(RawImport {
                spec,
                kind: ImportKind::From,
                line: fi.line as u32,
                statement: fi.statement.clone(),
                local_name: None,
                raw_path: Some(module),
            });
            continue;
        }
        for n in fi.names {
            let dotted = if fi.module.is_empty() {
                format!("{}{}", prefix, n.name)
            } else {
                format!("{}.{}", module, n.name)
            };
            let spec = normalise_python(&dotted, fi.relative_dots);
            out.push(RawImport {
                spec,
                kind: ImportKind::From,
                line: fi.line as u32,
                statement: fi.statement.clone(),
                local_name: n.alias,
                raw_path: Some(dotted),
            });
        }
    }
    // Bare `import x.y` — surface::imports doesn't extract these, so
    // we run a small ast-grep pass directly.
    out.extend(extract_python_bare(src));
    out
}

fn normalise_python(module: &str, dots: usize) -> String {
    if dots == 0 {
        // Absolute: `a.b.c` → `a/b/c`.
        return module.replace('.', "/");
    }
    // Relative: leading `.` or `..` becomes `./` or `../`.
    let body = module.trim_start_matches('.').replace('.', "/");
    let mut s = String::new();
    if dots == 1 {
        s.push_str("./");
    } else {
        for _ in 0..dots - 1 {
            s.push_str("../");
        }
    }
    s.push_str(&body);
    if s.ends_with('/') {
        s.pop();
    }
    s
}

fn extract_python_bare(src: &str) -> Vec<RawImport> {
    let mut out = Vec::new();
    let lang = SupportLang::Python;
    let ast = lang.ast_grep(src);
    let root = ast.root();
    _walk_python_bare(&root, &mut out);
    out
}

fn _walk_python_bare<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        if c.kind() == "import_statement" {
            // Children include `dotted_name` or `aliased_import` for each name.
            let line = (c.start_pos().line() + 1) as u32;
            let stmt = c.text().into_owned();
            for n in c.children() {
                let k = n.kind();
                if k == "dotted_name" {
                    let mod_name = n.text().into_owned();
                    out.push(RawImport {
                        spec: mod_name.replace('.', "/"),
                        kind: ImportKind::Bare,
                        line,
                        statement: stmt.clone(),
                        local_name: None,
                        raw_path: Some(mod_name),
                    });
                } else if k == "aliased_import" {
                    let name = n.field("name").map(|f| f.text().into_owned());
                    let alias = n.field("alias").map(|f| f.text().into_owned());
                    if let Some(name) = name {
                        out.push(RawImport {
                            spec: name.replace('.', "/"),
                            kind: ImportKind::Bare,
                            line,
                            statement: stmt.clone(),
                            local_name: alias,
                            raw_path: Some(name),
                        });
                    }
                }
            }
        }
    }
}

// ---- TS / JS ----

fn extract_typescript(src: &str, lang: Lang) -> Vec<RawImport> {
    let support = match lang {
        Lang::TypeScript => SupportLang::TypeScript,
        Lang::Tsx => SupportLang::Tsx,
        Lang::JavaScript => SupportLang::JavaScript,
        _ => return Vec::new(),
    };
    let ast = support.ast_grep(src);
    let root = ast.root();
    let mut out = Vec::new();
    _walk_ts(&root, &mut out);
    out
}

fn _walk_ts<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        let kind = c.kind();
        let kind = kind.as_ref();
        if kind == "import_statement" {
            consume_ts_import(&c, out);
        } else if kind == "export_statement" {
            // Re-exports are also dependency edges.
            consume_ts_export(&c, out);
        } else if kind == "expression_statement" {
            // Top-level CommonJS `require('x')` calls.
            consume_ts_require(&c, out);
        }
    }
}

fn consume_ts_import<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    let Some(source) = node.field("source") else {
        return;
    };
    let from = strip_quotes(&source.text());
    if from.is_empty() {
        return;
    }
    let line = (node.start_pos().line() + 1) as u32;
    let stmt = node.text().into_owned();

    // Detect what kind of import this is.
    let mut named_seen = false;
    let mut star_seen = false;
    for c in node.children() {
        let k = c.kind();
        if k == "import_clause" {
            for sub in c.children() {
                let sk = sub.kind();
                if sk == "named_imports" {
                    named_seen = true;
                } else if sk == "namespace_import" {
                    star_seen = true;
                }
            }
        }
    }
    let kind = if star_seen {
        ImportKind::StarFrom
    } else if named_seen {
        ImportKind::NamedFrom
    } else {
        ImportKind::Bare
    };
    out.push(RawImport {
        spec: from.clone(),
        kind,
        line,
        statement: stmt,
        local_name: None,
        raw_path: Some(from),
    });
}

fn consume_ts_export<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    let Some(source) = node.field("source") else {
        return;
    };
    let from = strip_quotes(&source.text());
    if from.is_empty() {
        return;
    }
    let line = (node.start_pos().line() + 1) as u32;
    let stmt = node.text().into_owned();
    let mut star = false;
    for c in node.children() {
        let k = c.kind();
        if k == "export_clause" {
            // `export { Foo } from 'x'`
            out.push(RawImport {
                spec: from.clone(),
                kind: ImportKind::NamedFrom,
                line,
                statement: stmt.clone(),
                local_name: None,
                raw_path: Some(from.clone()),
            });
            return;
        } else if k == "*" || k.as_ref() == "namespace_export" {
            star = true;
        }
    }
    out.push(RawImport {
        spec: from.clone(),
        kind: if star {
            ImportKind::StarFrom
        } else {
            ImportKind::NamedFrom
        },
        line,
        statement: stmt,
        local_name: None,
        raw_path: Some(from),
    });
}

fn consume_ts_require<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    // Look for `require('x')` or `require("x")` calls, top-level only.
    let text = node.text();
    let s = text.as_ref();
    if !s.contains("require(") {
        return;
    }
    let line = (node.start_pos().line() + 1) as u32;
    if let Some(start) = s.find("require(") {
        let after = &s[start + "require(".len()..];
        let arg = after.split(')').next().unwrap_or("").trim();
        let from = arg.trim_matches('\'').trim_matches('"');
        if !from.is_empty() && (from.starts_with('.') || !from.contains(' ')) {
            out.push(RawImport {
                spec: from.to_string(),
                kind: ImportKind::Bare,
                line,
                statement: s.to_string(),
                local_name: None,
                raw_path: Some(from.to_string()),
            });
        }
    }
}

fn strip_quotes(s: &str) -> String {
    let t = s.trim();
    // Length guard: a single `"` or `'` satisfies both starts_with and
    // ends_with against itself, but `t[1..t.len() - 1]` would panic on
    // `1..0`. Require a real pair (>= 2 bytes) before stripping.
    if t.len() >= 2
        && ((t.starts_with('"') && t.ends_with('"'))
            || (t.starts_with('\'') && t.ends_with('\'')))
    {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

// ---- Scala ----

fn extract_scala(src: &str) -> Vec<RawImport> {
    let lang = SupportLang::Scala;
    let ast = lang.ast_grep(src);
    let root = ast.root();
    let mut out = Vec::new();
    _walk_scala(&root, &mut out);
    out
}

fn _walk_scala<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        let kind = c.kind();
        let kind = kind.as_ref();
        if kind == "import_declaration" {
            // The import's text is `import a.b.c` or `import a.b.{c, d}`.
            let line = (c.start_pos().line() + 1) as u32;
            let stmt = c.text().into_owned();
            // Heuristic: take the part after `import `, split on `{` for selectors.
            let after_kw = stmt.trim_start_matches("import").trim_start();
            let (base, selectors) = match after_kw.split_once('{') {
                Some((a, sel)) => (a.trim().trim_end_matches('.').to_string(), Some(sel)),
                None => (after_kw.trim().to_string(), None),
            };
            if let Some(sel) = selectors {
                let inner = sel.trim_end_matches('}');
                for raw in inner.split(',') {
                    let part = raw.trim();
                    if part.is_empty() {
                        continue;
                    }
                    let (name, alias) = match part.split_once("=>") {
                        Some((n, a)) => (n.trim().to_string(), Some(a.trim().to_string())),
                        None => (part.to_string(), None),
                    };
                    let spec = if name == "_" {
                        base.clone()
                    } else {
                        format!("{}.{}", base, name)
                    };
                    out.push(RawImport {
                        spec: spec.replace('.', "/"),
                        kind: if name == "_" {
                            ImportKind::StarFrom
                        } else {
                            ImportKind::Bare
                        },
                        line,
                        statement: stmt.clone(),
                        local_name: alias,
                        raw_path: Some(spec),
                    });
                }
            } else {
                let kind = if base.ends_with("._") {
                    ImportKind::StarFrom
                } else {
                    ImportKind::Bare
                };
                let dotted = base.trim_end_matches("._").to_string();
                out.push(RawImport {
                    spec: dotted.replace('.', "/"),
                    kind,
                    line,
                    statement: stmt,
                    local_name: None,
                    raw_path: Some(dotted),
                });
            }
        } else if matches!(
            kind,
            "package_clause" | "object_definition" | "class_definition" | "trait_definition"
        ) {
            // Descend into bodies — Scala 3 allows nested imports.
            _walk_scala(&c, out);
        }
    }
}

// ---- Java ----

fn extract_java(src: &str) -> Vec<RawImport> {
    let lang = SupportLang::Java;
    let ast = lang.ast_grep(src);
    let root = ast.root();
    let mut out = Vec::new();
    for c in root.children() {
        if !c.is_named() {
            continue;
        }
        if c.kind() == "import_declaration" {
            let line = (c.start_pos().line() + 1) as u32;
            let stmt = c.text().into_owned();
            let body = stmt
                .trim_start_matches("import")
                .trim_end_matches(';')
                .trim();
            let is_static = body.starts_with("static ");
            let body = body.trim_start_matches("static ").trim();
            let is_glob = body.ends_with(".*");
            let dotted = body.trim_end_matches(".*").to_string();
            out.push(RawImport {
                spec: dotted.replace('.', "/"),
                kind: if is_static {
                    ImportKind::Static
                } else if is_glob {
                    ImportKind::Glob
                } else {
                    ImportKind::Bare
                },
                line,
                statement: stmt,
                local_name: None,
                raw_path: Some(dotted),
            });
        }
    }
    out
}

// ---- Kotlin ----

fn extract_kotlin(src: &str) -> Vec<RawImport> {
    let lang = SupportLang::Kotlin;
    let ast = lang.ast_grep(src);
    let root = ast.root();
    let mut out = Vec::new();
    _walk_kotlin(&root, &mut out);
    out
}

fn _walk_kotlin<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        let kind = c.kind();
        let kind = kind.as_ref();
        if kind == "import_header" || kind == "import_directive" || kind == "import_list" {
            // Some Kotlin tree-sitter grammars name it differently;
            // handle either. The text form is `import x.y.Z [as W]`.
            if kind == "import_list" {
                _walk_kotlin(&c, out);
                continue;
            }
            // Read the children rather than slicing the node's text: the
            // grammar swallows a comment that follows the statement into
            // the `import_header`, so `import a.b.C` written above a KDoc
            // block used to yield the path `a.b.C\n\n/** … */` and resolve
            // to nothing.
            let line = (c.start_pos().line() + 1) as u32;
            let mut dotted = String::new();
            let mut alias = None;
            let mut is_glob = false;
            for part in c.children() {
                match part.kind().as_ref() {
                    "identifier" => dotted = collapse_ws(&part.text()),
                    "wildcard_import" => is_glob = true,
                    "import_alias" => {
                        alias = part
                            .children()
                            .find(|a| a.kind() == "type_identifier")
                            .map(|a| collapse_ws(&a.text()))
                    }
                    _ => {}
                }
            }
            if dotted.is_empty() {
                continue;
            }
            let statement = if is_glob {
                format!("import {}.*", dotted)
            } else {
                match &alias {
                    Some(a) => format!("import {} as {}", dotted, a),
                    None => format!("import {}", dotted),
                }
            };
            out.push(RawImport {
                spec: dotted.replace('.', "/"),
                kind: if alias.is_some() {
                    ImportKind::Alias
                } else if is_glob {
                    ImportKind::Glob
                } else {
                    ImportKind::Bare
                },
                line,
                statement,
                local_name: alias,
                raw_path: Some(dotted),
            });
        }
    }
}

// ---- C# ----

fn extract_csharp(src: &str) -> Vec<RawImport> {
    let lang = SupportLang::CSharp;
    let ast = lang.ast_grep(src);
    let root = ast.root();
    let mut out = Vec::new();
    _walk_csharp(&root, &mut out);
    out
}

fn _walk_csharp<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        let kind = c.kind();
        let kind = kind.as_ref();
        if kind == "using_directive" {
            let line = (c.start_pos().line() + 1) as u32;
            let stmt = c.text().into_owned();
            let body = stmt.trim_start_matches("using").trim_end_matches(';').trim().to_string();
            let is_static = body.starts_with("static ");
            let rest = body.trim_start_matches("static ").trim().to_string();

            // Alias form: `A = X.Y` (no `static`).
            if !is_static {
                if let Some((alias, target)) = rest.split_once('=') {
                    let dotted = target.trim().to_string();
                    out.push(RawImport {
                        spec: dotted.replace('.', "/"),
                        kind: ImportKind::Alias,
                        line,
                        statement: stmt,
                        local_name: Some(alias.trim().to_string()),
                        raw_path: Some(dotted),
                    });
                    continue;
                }
            }

            let dotted = rest.to_string();
            out.push(RawImport {
                spec: dotted.replace('.', "/"),
                kind: if is_static {
                    ImportKind::Static
                } else {
                    ImportKind::Bare
                },
                line,
                statement: stmt,
                local_name: None,
                raw_path: Some(dotted),
            });
        } else if matches!(kind, "namespace_declaration" | "file_scoped_namespace_declaration") {
            // Recurse into namespace bodies; usings can live inside.
            _walk_csharp(&c, out);
        }
    }
}

// ---- Go ----

fn extract_go(src: &str) -> Vec<RawImport> {
    let lang = SupportLang::Go;
    let ast = lang.ast_grep(src);
    let root = ast.root();
    let mut out = Vec::new();
    for c in root.children() {
        if !c.is_named() {
            continue;
        }
        if c.kind() == "import_declaration" {
            // Either single `import "foo"` or grouped `import (...)`.
            for spec in c.children() {
                if !spec.is_named() {
                    continue;
                }
                if spec.kind() == "import_spec" {
                    consume_go_spec(&spec, &mut out);
                } else if spec.kind() == "import_spec_list" {
                    for inner in spec.children() {
                        if inner.is_named() && inner.kind() == "import_spec" {
                            consume_go_spec(&inner, &mut out);
                        }
                    }
                }
            }
        }
    }
    out
}

fn consume_go_spec<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    let line = (node.start_pos().line() + 1) as u32;
    let stmt = node.text().into_owned();
    let path = node.field("path").map(|f| f.text().into_owned());
    let name = node.field("name").map(|f| f.text().into_owned());
    let Some(path) = path else { return };
    let stripped = path.trim_matches('"').to_string();
    out.push(RawImport {
        spec: stripped.clone(),
        kind: ImportKind::Bare,
        line,
        statement: stmt,
        local_name: name,
        raw_path: Some(stripped),
    });
}

// ---- C++ ----

fn extract_cpp(src: &str) -> Vec<RawImport> {
    let lang = SupportLang::Cpp;
    let ast = lang.ast_grep(src);
    let root = ast.root();
    let mut out = Vec::new();
    _walk_cpp(&root, &mut out);
    out
}

fn _walk_cpp<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        let kind = c.kind();
        let kind = kind.as_ref();
        if kind == "preproc_include" {
            consume_cpp_include(&c, out);
        } else if matches!(
            kind,
            "namespace_definition"
                | "linkage_specification"
                | "translation_unit"
                | "preproc_ifdef"
                | "preproc_if"
                | "preproc_else"
                | "preproc_elif"
        ) {
            // Headers commonly wrap includes in `#ifndef … #endif` guards or
            // `extern "C" { … }`; namespaces also occasionally hold them.
            _walk_cpp(&c, out);
        }
    }
}

fn consume_cpp_include<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    let line = (node.start_pos().line() + 1) as u32;
    let stmt = node.text().into_owned();
    for sub in node.children() {
        if !sub.is_named() {
            continue;
        }
        let k = sub.kind();
        let k = k.as_ref();
        if k == "string_literal" {
            // `#include "local.h"` — local include, resolve relative.
            let raw = sub.text().into_owned();
            let path = raw.trim().trim_matches('"').to_string();
            if path.is_empty() {
                continue;
            }
            let spec = if path.starts_with("./") || path.starts_with("../") {
                path.clone()
            } else {
                format!("./{}", path)
            };
            out.push(RawImport {
                spec,
                kind: ImportKind::Bare,
                line,
                statement: stmt.clone(),
                local_name: None,
                raw_path: Some(path),
            });
        } else if k == "system_lib_string" {
            // `#include <vector>` — system header. Emit so it shows up in
            // external listings; the resolver won't find it inside the project.
            let raw = sub.text().into_owned();
            let inner = raw.trim().trim_start_matches('<').trim_end_matches('>').to_string();
            if inner.is_empty() {
                continue;
            }
            out.push(RawImport {
                spec: inner.clone(),
                kind: ImportKind::Bare,
                line,
                statement: stmt.clone(),
                local_name: None,
                raw_path: Some(format!("<{}>", inner)),
            });
        }
    }
}

// ---- PHP ----

fn extract_php(src: &str) -> Vec<RawImport> {
    let lang = SupportLang::Php;
    let ast = lang.ast_grep(src);
    let root = ast.root();
    let mut out = Vec::new();
    _walk_php(&root, &mut out);
    out
}

fn _walk_php<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        let kind = c.kind();
        let kind = kind.as_ref();
        if kind == "namespace_use_declaration" {
            consume_php_use(&c, out);
            continue;
        }
        if matches!(
            kind,
            "require_expression"
                | "require_once_expression"
                | "include_expression"
                | "include_once_expression"
        ) {
            consume_php_require(&c, out);
            continue;
        }
        // Imports can appear inside any nesting structure (class bodies,
        // method bodies, conditionals). Recurse through every named child;
        // the cost is negligible and matching is type-driven.
        _walk_php(&c, out);
    }
}

fn consume_php_use<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    let line = (node.start_pos().line() + 1) as u32;
    let stmt = node.text().into_owned();
    // `use App\Core\Foo;`, `use App\Core\Foo as Bar;`,
    // `use App\Core\{Foo, Bar};` — walk children for clauses.
    for sub in node.children() {
        if !sub.is_named() {
            continue;
        }
        let k = sub.kind();
        let k = k.as_ref();
        if k == "namespace_use_clause" || k == "namespace_use_group_clause" {
            let text = sub.text().into_owned();
            let (path, alias) = match text.split_once(" as ") {
                Some((p, a)) => (p.trim().to_string(), Some(a.trim().to_string())),
                None => (text.trim().to_string(), None),
            };
            let path = path.trim_start_matches('\\').to_string();
            if path.is_empty() {
                continue;
            }
            out.push(RawImport {
                spec: path.replace('\\', "/"),
                kind: ImportKind::Use,
                line,
                statement: stmt.clone(),
                local_name: alias,
                raw_path: Some(path),
            });
        } else if k == "namespace_use_group" {
            // `use App\Core\{Foo, Bar};` — find the prefix qualified_name and
            // each member clause inside the group.
            let mut prefix: Option<String> = None;
            for inner in sub.children() {
                if !inner.is_named() {
                    continue;
                }
                let ik = inner.kind();
                let ik = ik.as_ref();
                if (ik == "qualified_name" || ik == "namespace_name") && prefix.is_none() {
                    prefix = Some(inner.text().into_owned().trim_start_matches('\\').to_string());
                } else if ik == "namespace_use_group_clause" || ik == "namespace_use_clause" {
                    let text = inner.text().into_owned();
                    let (item, alias) = match text.split_once(" as ") {
                        Some((p, a)) => (p.trim().to_string(), Some(a.trim().to_string())),
                        None => (text.trim().to_string(), None),
                    };
                    let item = item.trim_start_matches('\\').to_string();
                    let full = match &prefix {
                        Some(p) if !p.is_empty() => format!("{}\\{}", p, item),
                        _ => item,
                    };
                    if full.is_empty() {
                        continue;
                    }
                    out.push(RawImport {
                        spec: full.replace('\\', "/"),
                        kind: ImportKind::Use,
                        line,
                        statement: stmt.clone(),
                        local_name: alias,
                        raw_path: Some(full),
                    });
                }
            }
        }
    }
}

fn consume_php_require<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    let line = (node.start_pos().line() + 1) as u32;
    let stmt = node.text().into_owned();
    // The argument is a string child — but `require('foo.php')` wraps the
    // string in a `parenthesized_expression`, so we may need to descend one
    // level. We bail on anything that's not a literal (concatenation,
    // variable, etc.) since dynamic paths can't be resolved statically.
    for sub in node.children() {
        if !sub.is_named() {
            continue;
        }
        if let Some(path) = _php_extract_string_arg(&sub) {
            if path.is_empty() {
                continue;
            }
            // Skip dynamic paths: __DIR__ concatenation and `$var`
            // interpolation never produce a static target.
            if path.contains("__DIR__") || path.contains('$') {
                continue;
            }
            let spec = if path.starts_with("./") || path.starts_with("../") {
                path.clone()
            } else {
                format!("./{}", path)
            };
            out.push(RawImport {
                spec,
                kind: ImportKind::Bare,
                line,
                statement: stmt.clone(),
                local_name: None,
                raw_path: Some(path),
            });
            break;
        }
    }
}

/// Pull the literal path out of a require/include argument, descending
/// through one level of `parenthesized_expression` if present. Returns
/// None for any non-literal form (concatenation, variable, function call).
fn _php_extract_string_arg<'a, D: Doc>(node: &Node<'a, D>) -> Option<String> {
    let k = node.kind();
    let k = k.as_ref();
    if k == "string" || k == "encapsed_string" {
        let raw = node.text().into_owned();
        return Some(
            raw.trim()
                .trim_matches('\'')
                .trim_matches('"')
                .to_string(),
        );
    }
    if k == "parenthesized_expression" {
        for inner in node.children() {
            if !inner.is_named() {
                continue;
            }
            if let Some(s) = _php_extract_string_arg(&inner) {
                return Some(s);
            }
        }
    }
    None
}

// ---- Ruby ----

fn extract_ruby(src: &str) -> Vec<RawImport> {
    let lang = SupportLang::Ruby;
    let ast = lang.ast_grep(src);
    let root = ast.root();
    let mut out = Vec::new();
    _walk_ruby(&root, &mut out);
    out
}

fn _walk_ruby<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        if c.kind().as_ref() == "call" {
            consume_ruby_call(&c, out);
            continue;
        }
        // `require_relative` calls can appear inside any block (class body,
        // method body, if/unless, begin/rescue). Walk the full tree —
        // `consume_ruby_call` filters out non-import calls cheaply.
        _walk_ruby(&c, out);
    }
}

fn consume_ruby_call<'a, D: Doc>(node: &Node<'a, D>, out: &mut Vec<RawImport>) {
    // tree-sitter-ruby exposes `method` and `arguments` fields on `call`.
    let method = match node.field("method") {
        Some(m) => m.text().into_owned(),
        None => return,
    };
    if !matches!(
        method.as_str(),
        "require" | "require_relative" | "load" | "autoload"
    ) {
        return;
    }
    let args = match node.field("arguments") {
        Some(a) => a,
        None => return,
    };
    // Collect named argument children in order. `autoload(const, file)` puts
    // the path in the SECOND positional slot; the rest put it first. Picking
    // by position handles `autoload "Foo", "path"` (where the const name is a
    // String) — naive "first string wins" would grab "Foo".
    let positional: Vec<_> = args.children().filter(|a| a.is_named()).collect();
    let path_arg = if method == "autoload" {
        positional.get(1)
    } else {
        positional.first()
    };
    let Some(path_arg) = path_arg else { return };
    if path_arg.kind().as_ref() != "string" {
        // Non-literal argument (variable, method call, interpolated string) —
        // can't resolve statically.
        return;
    }
    let raw = path_arg.text().into_owned();
    let path = raw
        .trim()
        .trim_matches('\'')
        .trim_matches('"')
        .to_string();
    if path.is_empty() {
        return;
    }
    let line = (node.start_pos().line() + 1) as u32;
    let stmt = node.text().into_owned();

    if method == "require_relative" {
        // Ruby convention is to omit the `.rb` extension; append it so the
        // resolver's `target.is_file()` check finds the actual file.
        let path_with_ext = if path.ends_with(".rb") {
            path.clone()
        } else {
            format!("{}.rb", path)
        };
        let spec = if path_with_ext.starts_with("./") || path_with_ext.starts_with("../") {
            path_with_ext.clone()
        } else {
            format!("./{}", path_with_ext)
        };
        out.push(RawImport {
            spec,
            kind: ImportKind::Bare,
            line,
            statement: stmt,
            local_name: None,
            raw_path: Some(path),
        });
    } else {
        // `require 'gem'`, `load`, `autoload` — these target $LOAD_PATH or
        // gems, which live outside the repo. Emit a non-resolvable spec so
        // they show up in the external list with the original path.
        out.push(RawImport {
            spec: path.clone(),
            kind: ImportKind::Bare,
            line,
            statement: stmt,
            local_name: None,
            raw_path: Some(path),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_quotes_handles_lone_quote_without_panic() {
        // Regression: a single `"` or `'` satisfies both starts_with and
        // ends_with against itself; without a length guard the slice
        // `t[1..t.len()-1]` would compute `1..0` and panic at runtime.
        // Real Scala/TS sources can produce lone-quote tokens when an
        // import path is malformed or a tree-sitter pass skips a child.
        assert_eq!(strip_quotes("\""), "\"");
        assert_eq!(strip_quotes("'"), "'");
        // Whitespace-padded lone quotes still hit the same code path.
        assert_eq!(strip_quotes("  \"  "), "\"");
    }

    #[test]
    fn strip_quotes_strips_paired_quotes() {
        assert_eq!(strip_quotes("\"hello\""), "hello");
        assert_eq!(strip_quotes("'world'"), "world");
        assert_eq!(strip_quotes("  \"x\"  "), "x");
    }

    #[test]
    fn strip_quotes_leaves_unquoted_unchanged() {
        assert_eq!(strip_quotes("foo"), "foo");
        assert_eq!(strip_quotes(""), "");
        // Mismatched delimiters: only `"x'` doesn't qualify as paired.
        assert_eq!(strip_quotes("\"x'"), "\"x'");
    }
}
