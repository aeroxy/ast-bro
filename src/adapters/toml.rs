use crate::core::{Declaration, DeclarationKind, ParseResult};
use std::path::Path;

fn byte_to_line(source: &[u8], byte_offset: usize) -> usize {
    let safe_offset = std::cmp::min(byte_offset, source.len());
    source[..safe_offset].iter().filter(|&&b| b == b'\n').count() + 1
}

pub fn parse_toml(path: &Path, source: &[u8]) -> ParseResult {
    let line_count = source.iter().filter(|&&b| b == b'\n').count() + 1;
    let mut decls = Vec::new();

    if let Ok(text) = std::str::from_utf8(source) {
        if let Ok(doc) = text.parse::<toml_edit::ImDocument<String>>() {
            // Walk top-level keys
            for (key_str, value) in doc.iter() {
                let key_name = key_str.to_string();
                let sig = _value_summary(value);

                let item_span = value.span().unwrap_or(0..0);
                let start_byte = item_span.start;
                let end_byte = if item_span.end > 0 { item_span.end } else { source.len() };

                let start_line = byte_to_line(source, start_byte);
                let end_line = byte_to_line(source, end_byte);

                let decl = Declaration {
                    kind: DeclarationKind::Field,
                    name: key_name,
                    signature: sig,
                    bases: Vec::new(),
                    attrs: Vec::new(),
                    docs: Vec::new(),
                    docs_inside: false,
                    visibility: String::new(),
                    start_line,
                    end_line,
                    start_byte,
                    end_byte,
                    doc_start_byte: start_byte,
                    native_kind: None,
                    modifiers: Vec::new(),
                    deprecated: false,
                    children: _table_children(value, source),
                    calls: Vec::new(),
                };
                decls.push(decl);
            }
        }
    }

    ParseResult {
        path: path.to_path_buf(),
        language: "toml",
        source: source.to_vec(),
        line_count,
        declarations: decls,
        error_count: 0,
        imports: Vec::new(),
    }
}

fn _table_children(value: &toml_edit::Item, source: &[u8]) -> Vec<Declaration> {
    match value {
        toml_edit::Item::Table(table) => {
            let mut children = Vec::new();
            for (key_str, val) in table.iter() {
                let val_span = val.span().unwrap_or(0..0);
                let start_byte = val_span.start;
                let end_byte = if val_span.end > 0 { val_span.end } else { start_byte };

                let start_line = byte_to_line(source, start_byte);
                let end_line = byte_to_line(source, end_byte);

                children.push(Declaration {
                    kind: DeclarationKind::Field,
                    name: key_str.to_string(),
                    signature: _value_summary(val),
                    bases: Vec::new(),
                    attrs: Vec::new(),
                    docs: Vec::new(),
                    docs_inside: false,
                    visibility: String::new(),
                    start_line,
                    end_line,
                    start_byte,
                    end_byte,
                    doc_start_byte: start_byte,
                    native_kind: None,
                    modifiers: Vec::new(),
                    deprecated: false,
                    children: Vec::new(),
                    calls: Vec::new(),
                });
            }
            children
        }
        _ => Vec::new(),
    }
}

fn _value_summary(value: &toml_edit::Item) -> String {
    match value {
        toml_edit::Item::None => "none".to_string(),
        toml_edit::Item::Value(v) => _scalar_summary(v),
        toml_edit::Item::Table(_) => "table".to_string(),
        toml_edit::Item::ArrayOfTables(_) => "array of tables".to_string(),
    }
}

fn _scalar_summary(value: &toml_edit::Value) -> String {
    match value {
        toml_edit::Value::String(s) => {
            let v = s.value();
            if v.len() > 40 {
                format!("\"...{}...\"", &v[..40])
            } else {
                format!("\"{}\"", v)
            }
        }
        toml_edit::Value::Integer(i) => format!("{}", i.value()),
        toml_edit::Value::Float(f) => format!("{}", f.value()),
        toml_edit::Value::Boolean(b) => format!("{}", b.value()),
        toml_edit::Value::Datetime(d) => d.to_string(),
        toml_edit::Value::Array(a) => format!("array[{}]", a.len()),
        toml_edit::Value::InlineTable(t) => format!("inline table[{}]", t.len()),
    }
}
