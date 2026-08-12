//! Visibility-only fallback for languages without re-export concepts
//! (Java, C#, Go, Kotlin, plus any source path the auto-detect couldn't
//! classify). The output is essentially `digest --no-private` rephrased
//! as `SurfaceEntry`s with package-qualified names derived from the
//! existing Namespace / Class ancestry already in the IR.

use crate::core::{Declaration, DeclarationKind};
use crate::surface::entry::SurfaceEntry;
use crate::surface::entry_point::EntryPoint;
use crate::surface::options::{SurfaceError, SurfaceOptions};
use crate::walk_and_parse;
use std::path::PathBuf;

pub fn resolve(
    entry: &EntryPoint,
    opts: &SurfaceOptions,
) -> Result<Vec<SurfaceEntry>, SurfaceError> {
    let paths = match entry {
        EntryPoint::Fallback { paths } => paths.clone(),
        _ => {
            return Err(SurfaceError::NoEntryPoint {
                path: PathBuf::from("."),
                hint: "fallback::resolve called with non-fallback entry point".into(),
            });
        }
    };

    let results = walk_and_parse(&paths, None);
    let mut out = Vec::new();
    for result in &results {
        for d in &result.declarations {
            _walk(d, &result.path, "", opts.include_private, &mut out);
        }
    }
    Ok(out)
}

fn _walk(
    decl: &Declaration,
    file: &std::path::Path,
    prefix: &str,
    include_private: bool,
    out: &mut Vec<SurfaceEntry>,
) {
    use DeclarationKind::*;
    let qname = if prefix.is_empty() {
        decl.name.clone()
    } else if decl.name.is_empty() {
        prefix.to_string()
    } else {
        format!("{}.{}", prefix, decl.name)
    };

    let is_namespace = matches!(decl.kind, Namespace);
    let visible = include_private || decl.visibility != "private";

    // Don't emit for the namespace shell itself, but pass its name into
    // the prefix used for children. For type-bearing decls, emit and recurse.
    if !is_namespace && visible && !decl.name.is_empty() {
        out.push(SurfaceEntry {
            qualified_path: qname.clone(),
            kind: decl.kind,
            signature: decl.signature.clone(),
            source_path: file.to_path_buf(),
            source_line: decl.start_line,
            source_name: decl.name.clone(),
            re_export_chain: Vec::new(),
            via_glob: false,
            docs: decl.docs.clone(),
            group: decl.group.clone(),
        });
    }

    let next_prefix = if matches!(decl.kind, Namespace | Class | Struct | Interface | Record | Enum)
    {
        qname
    } else {
        prefix.to_string()
    };
    for child in &decl.children {
        _walk(child, file, &next_prefix, include_private, out);
    }
}
