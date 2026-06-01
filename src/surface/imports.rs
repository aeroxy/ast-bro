//! Tree-sitter passes that the regular adapters drop.
//!
//! - Rust: `use_declaration` (with `pub`/`pub(crate)`/etc visibility) and
//!   `mod_item` declarations (without body — those are file references).
//!   The outline adapter picks up `mod_item` with body but not `use`.
//! - Python: `__all__` list assignments and `from X import Y` lines from
//!   `__init__.py`.
//!
//! These extractors return lightweight structs that the per-language
//! resolvers consume — they are *not* added to the outline IR (that
//! would change the public `ast-bro.outline.v1` JSON schema).

use ast_grep_core::{Doc, Node};
use ast_grep_language::{LanguageExt, SupportLang};

// ---- Rust ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UseSegmentKind {
    /// `pub use foo::Bar` (or with `as` rename via `alias`)
    Item,
    /// `pub use foo::*`
    Glob,
}

#[derive(Debug, Clone)]
pub struct UseItem {
    pub visibility: String,
    /// Path written in source, segments joined by `::`.
    /// For `pub use foo::bar::Baz`, this is `foo::bar::Baz`.
    /// For globs the trailing `*` is stripped (`foo::bar`).
    pub path: String,
    pub alias: Option<String>,
    pub kind: UseSegmentKind,
    pub line: usize,
    pub statement: String,
}

#[derive(Debug, Clone)]
pub struct ModRef {
    pub visibility: String,
    pub name: String,
    /// True if this is a stub `pub mod foo;` referencing an external file.
    /// False for inline `pub mod foo { ... }`.
    pub is_external_file: bool,
    #[allow(dead_code)]
    pub line: usize,
    /// `#[path = "..."]` override, if present on the immediately-preceding
    /// attribute. `None` means standard resolution rules apply.
    pub path_attr: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RustImports {
    pub uses: Vec<UseItem>,
    pub mods: Vec<ModRef>,
}

pub fn extract_rust_imports(src: &str) -> RustImports {
    let lang = SupportLang::Rust;
    let ast = lang.ast_grep(src);
    let root = ast.root();
    let mut out = RustImports::default();
    _walk_rust(&root, src, &mut out);
    out
}

fn _walk_rust<'a, D: Doc>(node: &Node<'a, D>, src: &str, out: &mut RustImports) {
    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        let kind = child.kind();
        let kind = kind.as_ref();
        if kind == "use_declaration" {
            _consume_use(&child, src, out);
        } else if kind == "mod_item" {
            _consume_mod(&child, src, out);
            // Inline `mod foo { pub use bar::*; }` — descend into body.
            if let Some(body) = child.field("body") {
                _walk_rust(&body, src, out);
            }
        }
    }
}

fn _consume_use<'a, D: Doc>(node: &Node<'a, D>, src: &str, out: &mut RustImports) {
    let vis = _rust_visibility(node);
    let argument = match node.field("argument") {
        Some(a) => a,
        None => return,
    };
    let line = node.start_pos().line() + 1;
    let stmt = _line_text(src, node.range().start, node.range().end);
    _flatten_use_tree(&argument, "", &vis, line, &stmt, out);
}

fn _flatten_use_tree<'a, D: Doc>(
    node: &Node<'a, D>,
    prefix: &str,
    vis: &str,
    line: usize,
    stmt: &str,
    out: &mut RustImports,
) {
    let kind = node.kind();
    match kind.as_ref() {
        "scoped_use_list" => {
            let path = node
                .field("path")
                .map(|p| p.text().into_owned())
                .unwrap_or_default();
            let new_prefix = if prefix.is_empty() {
                path
            } else {
                format!("{}::{}", prefix, path)
            };
            if let Some(list) = node.field("list") {
                for c in list.children() {
                    if c.is_named() {
                        _flatten_use_tree(&c, &new_prefix, vis, line, stmt, out);
                    }
                }
            }
        }
        "use_list" => {
            for c in node.children() {
                if c.is_named() {
                    _flatten_use_tree(&c, prefix, vis, line, stmt, out);
                }
            }
        }
        "use_wildcard" => {
            // Wildcard child path is the prefix to glob over.
            let path = node
                .children()
                .find(|c| c.is_named() && c.kind() != "*")
                .map(|c| c.text().into_owned())
                .unwrap_or_default();
            let full = if prefix.is_empty() {
                path
            } else if path.is_empty() {
                prefix.to_string()
            } else {
                format!("{}::{}", prefix, path)
            };
            out.uses.push(UseItem {
                visibility: vis.to_string(),
                path: full,
                alias: None,
                kind: UseSegmentKind::Glob,
                line,
                statement: stmt.to_string(),
            });
        }
        "use_as_clause" => {
            let path = node
                .field("path")
                .map(|p| p.text().into_owned())
                .unwrap_or_default();
            let alias = node.field("alias").map(|a| a.text().into_owned());
            let full = if prefix.is_empty() {
                path
            } else {
                format!("{}::{}", prefix, path)
            };
            out.uses.push(UseItem {
                visibility: vis.to_string(),
                path: full,
                alias,
                kind: UseSegmentKind::Item,
                line,
                statement: stmt.to_string(),
            });
        }
        "scoped_identifier" | "identifier" | "self" | "crate" | "super" => {
            let path = node.text().into_owned();
            let full = if prefix.is_empty() {
                path
            } else {
                format!("{}::{}", prefix, path)
            };
            out.uses.push(UseItem {
                visibility: vis.to_string(),
                path: full,
                alias: None,
                kind: UseSegmentKind::Item,
                line,
                statement: stmt.to_string(),
            });
        }
        _ => {}
    }
}

fn _consume_mod<'a, D: Doc>(node: &Node<'a, D>, src: &str, out: &mut RustImports) {
    let name = match node.field("name") {
        Some(n) => n.text().into_owned(),
        None => return,
    };
    let is_external_file = node.field("body").is_none();
    let path_attr = _find_path_attr(node, src);
    out.mods.push(ModRef {
        visibility: _rust_visibility(node),
        name,
        is_external_file,
        line: node.start_pos().line() + 1,
        path_attr,
    });
}

fn _find_path_attr<'a, D: Doc>(node: &Node<'a, D>, _src: &str) -> Option<String> {
    let mut current = node.prev();
    while let Some(prev) = current {
        let kind = prev.kind();
        if kind == "attribute_item" {
            let text = prev.text().into_owned();
            // Look for #[path = "..."]
            if let Some(start) = text.find("path") {
                let after = &text[start..];
                if let Some(eq) = after.find('=') {
                    let rest = after[eq + 1..].trim();
                    if let Some(s) = rest.strip_prefix('"').and_then(|s| s.split('"').next()) {
                        return Some(s.to_string());
                    }
                }
            }
            current = prev.prev();
            continue;
        }
        if kind == "line_comment" || kind == "block_comment" {
            current = prev.prev();
            continue;
        }
        break;
    }
    None
}

fn _rust_visibility<'a, D: Doc>(node: &Node<'a, D>) -> String {
    for c in node.children() {
        if c.kind() == "visibility_modifier" {
            return c.text().into_owned();
        }
    }
    String::new()
}

fn _line_text(src: &str, start: usize, end: usize) -> String {
    src.get(start..end)
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

// ---- Python ----

#[derive(Debug, Clone, Default)]
pub struct PythonImports {
    /// Names from `__all__ = [...]` if the assignment is found.
    pub dunder_all: Option<Vec<String>>,
    pub from_imports: Vec<FromImport>,
}

#[derive(Debug, Clone)]
pub struct FromImport {
    /// `module` part of `from <module> import ...`. Leading dots are
    /// stored in `relative_dots` and stripped from this field.
    pub module: String,
    pub relative_dots: usize,
    pub names: Vec<ImportedName>,
    pub line: usize,
    pub statement: String,
    pub is_glob: bool,
}

#[derive(Debug, Clone)]
pub struct ImportedName {
    pub name: String,
    pub alias: Option<String>,
}

pub fn extract_python_imports(src: &str) -> PythonImports {
    let lang = SupportLang::Python;
    let ast = lang.ast_grep(src);
    let root = ast.root();
    let mut out = PythonImports::default();
    _walk_python(&root, src, &mut out);
    out
}

fn _walk_python<'a, D: Doc>(node: &Node<'a, D>, src: &str, out: &mut PythonImports) {
    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        let kind = child.kind();
        if kind == "import_from_statement" {
            if let Some(fi) = _parse_from_import(&child, src) {
                out.from_imports.push(fi);
            }
        } else if kind == "expression_statement" {
            for inner in child.children() {
                if inner.kind() == "assignment" {
                    if let Some(items) = _parse_dunder_all(&inner) {
                        out.dunder_all = Some(items);
                    }
                }
            }
        }
    }
}

fn _parse_from_import<'a, D: Doc>(node: &Node<'a, D>, src: &str) -> Option<FromImport> {
    // Tree-sitter Python `import_from_statement`:
    //   `module_name` field for the source
    //   `name` field (repeated) for each imported name (`dotted_name` or
    //    `aliased_import`), or a `wildcard_import` child for `*`.
    let module_field = node.field("module_name");
    let mut dots = 0usize;
    let mut module_text = String::new();
    if let Some(mn) = &module_field {
        let mk = mn.kind();
        match mk.as_ref() {
            "relative_import" => {
                for c in mn.children() {
                    let kind = c.kind();
                    if kind == "import_prefix" {
                        // text is something like "." or ".." — count chars
                        dots = c.text().chars().filter(|ch| *ch == '.').count();
                    } else if c.is_named() {
                        let t = c.text().into_owned();
                        if !t.is_empty() {
                            module_text = t;
                        }
                    }
                }
            }
            _ => {
                module_text = mn.text().into_owned();
            }
        }
    }

    let line = node.start_pos().line() + 1;
    let stmt = _line_text(src, node.range().start, node.range().end);

    // Glob?
    let mut is_glob = false;
    let mut names = Vec::new();
    for c in node.children() {
        let kind = c.kind();
        if kind == "wildcard_import" {
            is_glob = true;
        } else if kind == "dotted_name" || kind == "identifier" {
            // Skip the module-name node we already consumed.
            if let Some(mn) = &module_field {
                if c.range() == mn.range() {
                    continue;
                }
            }
            names.push(ImportedName {
                name: c.text().into_owned(),
                alias: None,
            });
        } else if kind == "aliased_import" {
            let n = c.field("name").map(|f| f.text().into_owned());
            let a = c.field("alias").map(|f| f.text().into_owned());
            if let Some(name) = n {
                names.push(ImportedName { name, alias: a });
            }
        }
    }

    Some(FromImport {
        module: module_text,
        relative_dots: dots,
        names,
        line,
        statement: stmt,
        is_glob,
    })
}

// ---- TypeScript / JavaScript ----

#[derive(Debug, Clone)]
pub struct TsExports {
    pub items: Vec<TsExportItem>,
}

#[derive(Debug, Clone)]
pub enum TsExportItem {
    /// `export class/function/const/type/interface/enum X { ... }` —
    /// the name is what's exported. Source is this same file.
    Local {
        name: String,
        #[allow(dead_code)]
        line: usize,
    },
    /// `export default ...` — exposes the default binding.
    Default {
        /// If the default declaration has a name (`export default class X`),
        /// the name. Otherwise the literal "default".
        name: String,
        #[allow(dead_code)]
        line: usize,
    },
    /// `export { local, other as renamed }` — re-export local bindings.
    Named {
        bindings: Vec<NamedBinding>,
        #[allow(dead_code)]
        line: usize,
        #[allow(dead_code)]
        statement: String,
    },
    /// `export { Foo, Bar as Baz } from './x'`
    NamedFrom {
        from: String,
        bindings: Vec<NamedBinding>,
        line: usize,
        statement: String,
    },
    /// `export * from './x'`
    StarFrom {
        from: String,
        line: usize,
        statement: String,
    },
    /// `export * as ns from './x'`
    NamespaceFrom {
        ns: String,
        from: String,
        line: usize,
        statement: String,
    },
}

#[derive(Debug, Clone)]
pub struct NamedBinding {
    pub name: String,
    pub alias: Option<String>,
}

pub fn extract_ts_exports(src: &str, ts_kind: TsKind) -> TsExports {
    let lang = match ts_kind {
        TsKind::TypeScript => SupportLang::TypeScript,
        TsKind::Tsx => SupportLang::Tsx,
        TsKind::JavaScript => SupportLang::JavaScript,
    };
    let ast = lang.ast_grep(src);
    let root = ast.root();
    let mut items = Vec::new();
    for child in root.children() {
        if !child.is_named() {
            continue;
        }
        if child.kind() == "export_statement" {
            _consume_export(&child, src, &mut items);
        }
    }
    TsExports { items }
}

#[derive(Debug, Clone, Copy)]
pub enum TsKind {
    TypeScript,
    Tsx,
    JavaScript,
}

impl TsKind {
    pub fn from_path(p: &std::path::Path) -> Option<Self> {
        let ext = p.extension().and_then(|s| s.to_str())?;
        Some(match ext {
            "ts" | "mts" | "cts" | "d.ts" => Self::TypeScript,
            "tsx" => Self::Tsx,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            _ => return None,
        })
    }
}

fn _consume_export<'a, D: Doc>(node: &Node<'a, D>, src: &str, out: &mut Vec<TsExportItem>) {
    let line = node.start_pos().line() + 1;
    let stmt = _line_text(src, node.range().start, node.range().end);

    // Find the "from '...'" string sibling, if any.
    let mut from_module: Option<String> = None;
    for c in node.children() {
        if c.kind() == "string" {
            let raw = c.text().into_owned();
            from_module = Some(raw.trim_matches(|ch: char| ch == '\'' || ch == '"').to_string());
            break;
        }
    }

    // Detect `default` keyword for `export default ...`.
    let mut is_default = false;
    let mut star_seen = false;
    let mut star_alias: Option<String> = None;
    let mut after_star_as = false;
    for c in node.children() {
        let k = c.kind();
        let k = k.as_ref();
        if k == "default" {
            is_default = true;
        } else if k == "*" {
            star_seen = true;
            after_star_as = false;
        } else if k == "as" && star_seen {
            after_star_as = true;
        } else if after_star_as && c.is_named() {
            star_alias = Some(c.text().into_owned());
            after_star_as = false;
        }
    }

    // Collect `export_clause` if present.
    let clause = node.children().find(|c| c.kind() == "export_clause");
    if let Some(cl) = clause {
        let bindings = _parse_export_clause(&cl);
        if let Some(from) = from_module.clone() {
            out.push(TsExportItem::NamedFrom {
                from,
                bindings,
                line,
                statement: stmt.clone(),
            });
        } else {
            out.push(TsExportItem::Named {
                bindings,
                line,
                statement: stmt.clone(),
            });
        }
        return;
    }

    if star_seen {
        if let Some(from) = from_module {
            if let Some(ns) = star_alias {
                out.push(TsExportItem::NamespaceFrom {
                    ns,
                    from,
                    line,
                    statement: stmt,
                });
            } else {
                out.push(TsExportItem::StarFrom {
                    from,
                    line,
                    statement: stmt,
                });
            }
        }
        return;
    }

    if is_default {
        // `export default class Foo {}` — try to name it.
        let name = node
            .children()
            .filter(|c| c.is_named())
            .find_map(|c| {
                let kind = c.kind();
                if matches!(
                    kind.as_ref(),
                    "class_declaration"
                        | "function_declaration"
                        | "abstract_class_declaration"
                        | "identifier"
                ) {
                    c.field("name")
                        .map(|f| f.text().into_owned())
                        .or_else(|| Some(c.text().into_owned()))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "default".to_string());
        out.push(TsExportItem::Default { name, line });
        return;
    }

    // `export class Foo {}` / `export function bar()` / `export const x` etc.
    for raw_c in node.children() {
        if !raw_c.is_named() {
            continue;
        }
        // Step through `export declare class/...` ambient wrappers.
        let c = if raw_c.kind() == "ambient_declaration" {
            raw_c
                .children()
                .find(|cc| {
                    cc.is_named()
                        && matches!(
                            cc.kind().as_ref(),
                            "class_declaration"
                                | "abstract_class_declaration"
                                | "interface_declaration"
                                | "enum_declaration"
                                | "type_alias_declaration"
                                | "function_declaration"
                                | "function_signature"
                                | "lexical_declaration"
                                | "variable_declaration"
                        )
                })
                .unwrap_or_else(|| raw_c.clone())
        } else {
            raw_c
        };
        let kind = c.kind();
        match kind.as_ref() {
            "class_declaration" | "abstract_class_declaration" | "interface_declaration"
            | "enum_declaration" | "type_alias_declaration" | "function_declaration"
            | "function_signature" => {
                if let Some(n) = c.field("name") {
                    out.push(TsExportItem::Local {
                        name: n.text().into_owned(),
                        line,
                    });
                }
            }
            "lexical_declaration" | "variable_declaration" => {
                for d in c.children() {
                    if d.kind() == "variable_declarator" {
                        if let Some(n) = d.field("name") {
                            if n.kind() == "identifier" {
                                out.push(TsExportItem::Local {
                                    name: n.text().into_owned(),
                                    line,
                                });
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn _parse_export_clause<'a, D: Doc>(clause: &Node<'a, D>) -> Vec<NamedBinding> {
    let mut out = Vec::new();
    for spec in clause.children() {
        if spec.kind() != "export_specifier" {
            continue;
        }
        let name = spec
            .field("name")
            .map(|n| n.text().into_owned())
            .unwrap_or_default();
        let alias = spec.field("alias").map(|n| n.text().into_owned());
        if !name.is_empty() {
            out.push(NamedBinding { name, alias });
        }
    }
    out
}

// ---- Scala 3 ----

#[derive(Debug, Clone)]
pub struct ScalaExports {
    pub items: Vec<ScalaExportItem>,
    /// Top-level package declaration if any (`package foo.bar`).
    pub package: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ScalaExportItem {
    /// Source object/value path (e.g. `foo.bar` in `export foo.bar.{X, Y}`).
    pub from: String,
    /// Specific names exported. Empty when `kind == Glob`.
    pub bindings: Vec<NamedBinding>,
    pub kind: ScalaExportKind,
    pub line: usize,
    pub statement: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScalaExportKind {
    /// `export foo.Bar` or `export foo.{A, B as C}`
    Selectors,
    /// `export foo.*`
    Glob,
}

pub fn extract_scala_exports(src: &str) -> ScalaExports {
    let lang = SupportLang::Scala;
    let ast = lang.ast_grep(src);
    let root = ast.root();
    let mut items = Vec::new();
    let mut package = None;
    _walk_scala(&root, src, &mut items, &mut package);
    ScalaExports { items, package }
}

fn _walk_scala<'a, D: Doc>(
    node: &Node<'a, D>,
    src: &str,
    out: &mut Vec<ScalaExportItem>,
    package: &mut Option<String>,
) {
    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        let kind = child.kind();
        let kind = kind.as_ref();
        if kind == "package_clause" {
            // Capture the first package declaration only.
            if package.is_none() {
                let mut name_parts = Vec::new();
                for c in child.children() {
                    if c.kind() == "package_identifier" || c.kind() == "identifier" {
                        name_parts.push(c.text().into_owned());
                    }
                }
                if !name_parts.is_empty() {
                    *package = Some(name_parts.join("."));
                }
            }
            _walk_scala(&child, src, out, package);
        } else if kind == "export_declaration" || kind == "export_clause" {
            if let Some(item) = _parse_scala_export(&child, src) {
                out.push(item);
            }
        } else {
            // Descend into anything else — `template_body`,
            // `object_definition`, `class_definition`, etc. all
            // potentially contain `export` clauses inside.
            _walk_scala(&child, src, out, package);
        }
    }
}

fn _parse_scala_export<'a, D: Doc>(node: &Node<'a, D>, src: &str) -> Option<ScalaExportItem> {
    let line = node.start_pos().line() + 1;
    let stmt = _line_text(src, node.range().start, node.range().end);

    // The grammar for `export` looks like:
    //   "export" stable_identifier ( "." selector_list )?
    // where stable_identifier is a dotted path and selector_list is one
    // of: a single identifier, `_` (Scala 2) / `*` (Scala 3) for glob,
    // or `{ Foo, Bar => Baz, ... }`.
    //
    // Tree-sitter-scala's exact node names vary by version. We extract
    // by inspecting children textually rather than relying on field
    // names.

    let mut path_parts: Vec<String> = Vec::new();
    let mut bindings: Vec<NamedBinding> = Vec::new();
    let mut is_glob = false;

    for c in node.children() {
        if !c.is_named() {
            continue;
        }
        let kind = c.kind();
        let kind = kind.as_ref();
        match kind {
            "stable_identifier" | "identifier" | "package_identifier" => {
                // Each `identifier` child is one segment of the dotted
                // export path. tree-sitter-scala emits the segments as
                // separate `identifier` siblings interleaved with `.`
                // tokens, so we collect them in source order.
                let t = c.text().into_owned();
                if !t.is_empty() {
                    path_parts.extend(t.split('.').map(|s| s.to_string()));
                }
            }
            "wildcard" | "wildcard_import" | "namespace_wildcard" => {
                is_glob = true;
            }
            "import_selectors" | "given_imports" | "import_selector_list" => {
                for s in c.children() {
                    if !s.is_named() {
                        continue;
                    }
                    let sk = s.kind();
                    let sk = sk.as_ref();
                    if matches!(sk, "wildcard" | "wildcard_import" | "namespace_wildcard") {
                        is_glob = true;
                    } else if sk == "import_selector" || sk == "renamed_identifier" {
                        let mut name = String::new();
                        let mut alias: Option<String> = None;
                        for inner in s.children() {
                            if !inner.is_named() {
                                continue;
                            }
                            let ik = inner.kind();
                            let ik = ik.as_ref();
                            if ik == "identifier" {
                                if name.is_empty() {
                                    name = inner.text().into_owned();
                                } else {
                                    alias = Some(inner.text().into_owned());
                                }
                            }
                        }
                        if !name.is_empty() {
                            bindings.push(NamedBinding { name, alias });
                        }
                    } else if sk == "identifier" {
                        bindings.push(NamedBinding {
                            name: s.text().into_owned(),
                            alias: None,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    let from = path_parts.join(".");
    if from.is_empty() {
        return None;
    }
    let kind = if is_glob {
        ScalaExportKind::Glob
    } else {
        ScalaExportKind::Selectors
    };
    Some(ScalaExportItem {
        from,
        bindings,
        kind,
        line,
        statement: stmt,
    })
}

fn _parse_dunder_all<'a, D: Doc>(assignment: &Node<'a, D>) -> Option<Vec<String>> {
    let left = assignment.field("left")?;
    if left.kind() != "identifier" || left.text() != "__all__" {
        return None;
    }
    let right = assignment.field("right")?;
    let mut out = Vec::new();
    for c in right.children() {
        if c.kind() == "string" {
            let raw = c.text().into_owned();
            let trimmed = raw
                .trim_matches(|ch: char| ch == '"' || ch == '\'')
                .to_string();
            out.push(trimmed);
        }
    }
    Some(out)
}

