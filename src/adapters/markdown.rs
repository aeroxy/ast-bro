use crate::core::{Declaration, DeclarationKind, ParseResult};
use std::path::Path;

pub fn parse_markdown(path: &Path, source: &[u8]) -> ParseResult {
    // Instead of implementing Language and LanguageExt, we can use ast_grep_core::tree_sitter directly
    // to just parse the AST and build a mock Root or Node.
    // However, to keep it simple and fit the architecture, we will use Html SupportLang
    // to get a generic Doc instance and parse manually using tree_sitter-md.

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_md::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();

    let mut decls = Vec::new();

    // Frontmatter first, so it leads the outline the way it leads the file.
    if let Some(fm) = _frontmatter_decl(source) {
        decls.push(fm);
    }

    // Instead of ast_grep Nodes, we have to use tree_sitter nodes and convert manually
    // or implement the full tree-sitter walk without ast_grep abstraction for this one file.
    _walk_ts(tree.root_node(), source, &mut decls);

    ParseResult {
        path: path.to_path_buf(),
        language: "markdown",
        source: source.to_vec(),
        line_count: source.iter().filter(|&&b| b == b'\n').count() + 1,
        declarations: decls,
        error_count: 0, // Simplified for manual ts
        imports: Vec::new(),
    }
}

/// Detect a leading YAML frontmatter block and turn it into one declaration.
///
/// A `---…---` metadata block is where a task card, a Jekyll / Hugo / Astro /
/// Docusaurus page or an Obsidian note keeps its actual signal, and until now
/// it was invisible: a file whose whole content is frontmatter mapped as
/// empty. It is deliberately kept opaque — one node spanning the block, with
/// the keys left inside rather than parsed into a tree — because the payload
/// is arbitrary YAML and `show card.md frontmatter` returns the raw text
/// anyway, values included.
///
/// Only a `---` fence on the file's very first line counts. A `---` further
/// down is an ordinary horizontal rule and stays one; without that anchor,
/// any document using thematic breaks would sprout phantom metadata. A UTF-8
/// BOM before the opening fence is skipped (editors on Windows add one) and
/// trailing whitespace on either fence is tolerated, since every generator
/// that reads frontmatter tolerates it.
///
/// Scans bytes rather than `str`: only the fence lines are ever compared, so
/// there is no reason to make the whole file's encoding a precondition for
/// finding its metadata.
fn _frontmatter_decl(src: &[u8]) -> Option<Declaration> {
    /// Strip the line terminator and any trailing spaces/tabs.
    fn fence(line: &[u8]) -> &[u8] {
        let mut end = line.len();
        while end > 0 && matches!(line[end - 1], b'\n' | b'\r' | b' ' | b'\t') {
            end -= 1;
        }
        &line[..end]
    }

    // A BOM sits before the fence, so the fence is still "on line 1".
    const BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
    let bom = if src.starts_with(BOM) { BOM.len() } else { 0 };
    let mut lines = src[bom..].split_inclusive(|&b| b == b'\n');

    let first = lines.next()?;
    if fence(first) != b"---" {
        return None;
    }

    let mut offset = bom + first.len();
    let mut line_no = 1usize; // 1-indexed; the opening fence is line 1
    for line in lines {
        line_no += 1;
        let trimmed = fence(line);
        offset += line.len();
        // `...` is YAML's alternative document terminator, accepted by every
        // static-site generator that reads frontmatter.
        if trimmed == b"---" || trimmed == b"..." {
            // End at the closing fence, not past its newline: every other
            // declaration's range stops at its last non-terminator byte, and
            // including it would make `show` print a trailing blank line.
            let offset = offset - (line.len() - trimmed.len());
            return Some(Declaration {
                kind: DeclarationKind::Frontmatter,
                name: "frontmatter".to_string(),
                signature: "--- frontmatter".to_string(),
                bases: Vec::new(),
                attrs: Vec::new(),
                docs: Vec::new(),
                docs_inside: false,
                group: None,
                visibility: "public".to_string(),
                start_line: 1,
                end_line: line_no,
                // After the BOM, if any: the extracted block should be the
                // fence and its keys, not a stray byte-order mark.
                start_byte: bom,
                end_byte: offset,
                doc_start_byte: bom,
                native_kind: None,
                modifiers: Vec::new(),
                deprecated: false,
                children: Vec::new(),
                calls: Vec::new(),
            });
        }
    }
    // An opening fence with no closing one is not frontmatter — reporting it
    // would claim a block whose extent we cannot know.
    None
}

fn _walk_ts(node: tree_sitter::Node, src: &[u8], out: &mut Vec<Declaration>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "section" {
            if let Some(decl) = _section_to_decl_ts(child, src) {
                out.push(decl);
            }
        } else if child.kind() == "fenced_code_block" {
            out.push(_code_block_to_decl_ts(child, src));
        }
    }
}

fn _section_to_decl_ts(node: tree_sitter::Node, src: &[u8]) -> Option<Declaration> {
    let heading = _find_heading_ts(node)?;
    let (level, title) = _heading_level_and_title_ts(heading, src);

    let mut signature = String::new();
    for _ in 0..level {
        signature.push('#');
    }
    if !title.is_empty() {
        signature.push(' ');
        signature.push_str(&title);
    }

    let mut children = Vec::new();
    let mut seen_heading = false;

    let mut cursor = node.walk();
    for c in node.named_children(&mut cursor) {
        if c.start_byte() == heading.start_byte() {
            seen_heading = true;
            continue;
        }

        let k = c.kind();
        if k == "section" {
            if let Some(sub) = _section_to_decl_ts(c, src) {
                children.push(sub);
            }
        } else if k == "fenced_code_block" {
            children.push(_code_block_to_decl_ts(c, src));
        } else if (k == "atx_heading" || k == "setext_heading") && seen_heading {
            if let Some(pseudo) = _pseudo_section_from_heading_ts(c, node, src) {
                children.push(pseudo);
            }
        }
    }

    Some(Declaration {
        kind: DeclarationKind::Heading,
        name: if title.is_empty() {
            "?".to_string()
        } else {
            title
        },
        signature,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
        group: None,
        visibility: "public".to_string(),
        start_line: node.start_position().row + 1,
        end_line: _end_line_ts(node),
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        doc_start_byte: node.start_byte(),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children,
        calls: Vec::new(),
    })
}

fn _pseudo_section_from_heading_ts(
    heading: tree_sitter::Node,
    parent_section: tree_sitter::Node,
    src: &[u8],
) -> Option<Declaration> {
    let (level, title) = _heading_level_and_title_ts(heading, src);
    let mut signature = String::new();
    for _ in 0..level {
        signature.push('#');
    }
    if !title.is_empty() {
        signature.push(' ');
        signature.push_str(&title);
    }

    let mut end_byte = parent_section.end_byte();
    let mut end_line = _end_line_ts(parent_section);
    let mut found_self = false;

    let mut cursor = parent_section.walk();
    for later in parent_section.named_children(&mut cursor) {
        if !found_self {
            if later.start_byte() == heading.start_byte() {
                found_self = true;
            }
            continue;
        }
        let k = later.kind();
        if k == "atx_heading" || k == "setext_heading" || k == "section" {
            end_byte = later.start_byte();
            end_line = later.start_position().row + 1;
            break;
        }
    }

    if !found_self {
        return None;
    }

    Some(Declaration {
        kind: DeclarationKind::Heading,
        name: if title.is_empty() {
            "?".to_string()
        } else {
            title
        },
        signature,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
        group: None,
        visibility: "public".to_string(),
        start_line: heading.start_position().row + 1,
        end_line,
        start_byte: heading.start_byte(),
        end_byte,
        doc_start_byte: heading.start_byte(),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    })
}

fn _code_block_to_decl_ts(node: tree_sitter::Node, src: &[u8]) -> Declaration {
    let info = _info_string_ts(node, src).unwrap_or_else(|| "code".to_string());
    let signature = format!("{} code block", info);
    Declaration {
        kind: DeclarationKind::CodeBlock,
        name: info,
        signature,
        bases: Vec::new(),
        attrs: Vec::new(),
        docs: Vec::new(),
        docs_inside: false,
        group: None,
        visibility: "public".to_string(),
        start_line: node.start_position().row + 1,
        end_line: _end_line_ts(node),
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        doc_start_byte: node.start_byte(),
        native_kind: None,
        modifiers: Vec::new(),
        deprecated: false,
        children: Vec::new(),
        calls: Vec::new(),
    }
}

fn _end_line_ts(node: tree_sitter::Node) -> usize {
    let end_pos = node.end_position();
    let mut end_row = end_pos.row;
    let end_col = end_pos.column;

    if end_col == 0 && end_row > node.start_position().row {
        end_row -= 1;
    }
    end_row + 1
}

fn _find_heading_ts(section: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut cursor = section.walk();
    let ret = section.named_children(&mut cursor).find(|&c| c.kind() == "atx_heading" || c.kind() == "setext_heading");
    ret
}

fn _heading_level_and_title_ts(heading: tree_sitter::Node, src: &[u8]) -> (usize, String) {
    if heading.kind() == "atx_heading" {
        let mut level = 1;
        let mut cursor = heading.walk();
        for c in heading.children(&mut cursor) {
            let k = c.kind();
            if k.starts_with("atx_h") && k.ends_with("_marker") {
                if let Ok(l) = k["atx_h".len().."atx_h".len() + 1].parse::<usize>() {
                    level = l;
                }
                break;
            }
        }

        let mut cursor2 = heading.walk();
        let inline = heading
            .named_children(&mut cursor2)
            .find(|c| c.kind() == "inline");
        let title = inline
            .map(|i| {
                String::from_utf8_lossy(&src[i.start_byte()..i.end_byte()])
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();
        return (level, title);
    }

    if heading.kind() == "setext_heading" {
        let mut level = 2;
        let mut cursor = heading.walk();
        for c in heading.children(&mut cursor) {
            if c.kind() == "setext_h1_underline" {
                level = 1;
                break;
            }
            if c.kind() == "setext_h2_underline" {
                level = 2;
                break;
            }
        }

        let mut cursor2 = heading.walk();
        let paragraph = heading
            .named_children(&mut cursor2)
            .find(|c| c.kind() == "paragraph");
        let title = paragraph
            .map(|p| {
                String::from_utf8_lossy(&src[p.start_byte()..p.end_byte()])
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();
        return (level, title);
    }

    (1, String::new())
}

fn _info_string_ts(fenced: tree_sitter::Node, src: &[u8]) -> Option<String> {
    let mut cursor = fenced.walk();
    for c in fenced.named_children(&mut cursor) {
        if c.kind() == "info_string" {
            return Some(
                String::from_utf8_lossy(&src[c.start_byte()..c.end_byte()])
                    .trim()
                    .to_string(),
            );
        }
    }
    None
}
