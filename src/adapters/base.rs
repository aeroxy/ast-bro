use crate::core::ParseResult;
use ast_grep_core::{Doc, Node};
use std::path::Path;

pub trait LanguageAdapter {
  fn language_name(&self) -> &'static str;

  /// Parses the file content, using an initial root Node parsed by ast_grep
  fn parse<'a, D: Doc>(&self, path: &Path, source: &[u8], root: Node<'a, D>) -> ParseResult;
}

pub fn count_parse_errors<D: Doc>(root: Node<D>) -> usize {
  let mut total = 0;
  let mut stack = vec![root];
  while let Some(n) = stack.pop() {
    if n.kind() == "ERROR" || n.is_missing() {
      total += 1;
    }
    for child in n.children() {
      stack.push(child);
    }
  }
  total
}

pub fn field_text<'a, D: Doc>(node: &Node<'a, D>, field_name: &str) -> Option<String> {
  node.field(field_name).map(|n| n.text().into_owned())
}

pub fn collapse_ws(text: &str) -> String {
  text.split_whitespace().collect::<Vec<_>>().join(" ")
}
