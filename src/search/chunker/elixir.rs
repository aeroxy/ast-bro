//! Elixir member walk.
//!
//! The generic walk recognises members by node kind, and tree-sitter-elixir
//! has none to recognise: `defmodule`, `def` and `test "…" do` are all a
//! `call` carrying a `do_block`, told apart only by the macro their `target`
//! names. Left to the generic walk, a module was one chunk however large —
//! the defk compiler's 6,475 lines came out as a single 234 KB chunk, far past
//! what the embedder keeps.
//!
//! This walk reads the macro name instead, through the same recognition the
//! `map` adapter uses ([`definition`]), and fills the same [`SplitPlan`]:
//!
//! - `defmodule` / `defprotocol` / `defimpl` is a member that is opened, and
//!   its name becomes the breadcrumb of everything inside it.
//! - A `def`-family clause is a callable member. Adjacent clauses of one
//!   name/arity are one outline entry, but each clause stays a split point, so
//!   a forty-clause function still packs into chunks the embedder accepts.
//! - Any other macro call with a `do` block directly in a module body (`test`,
//!   `describe`, `setup`, `schema`) is a callable member too, so a test module
//!   splits per test. One whose block holds definitions (`if … do def …`,
//!   `for … do def …`) is a wrapper instead, opened without a level of its own.
use super::*;
use crate::adapters::elixir::{definition, Definition};
use ast_grep_core::{Doc, Node};

/// What one form in a body is to the member walk.
enum Form {
    /// A module-like definition: a member whose body is walked.
    Module {
        name: String,
        signature: String,
    },
    /// A function clause: a callable member, folded with its neighbours.
    Clause {
        key: (String, String, usize),
        name: String,
        signature: String,
    },
    /// A `do`-block macro call holding no definitions: a callable member with
    /// no name of its own.
    Block {
        signature: String,
    },
    /// A `do`-block macro call holding definitions: walked through, not a
    /// member.
    Wrapper,
    Other,
}

fn form_of<D: Doc>(node: &Node<'_, D>, source: &str) -> Form {
    match definition(node) {
        Some(Definition::Module { name, signature }) => return Form::Module { name, signature },
        Some(Definition::Function {
            keyword,
            name,
            arity,
            signature,
        }) => {
            return Form::Clause {
                key: (keyword, name.clone(), arity),
                name,
                signature,
            }
        }
        None => {}
    }
    if node.kind() != "call"
        || node
            .field("target")
            .is_none_or(|t| t.kind() != "identifier")
    {
        return Form::Other;
    }
    let Some(block) = do_block(node) else {
        return Form::Other;
    };
    if block.children().any(|c| definition(&c).is_some()) {
        Form::Wrapper
    } else {
        Form::Block {
            signature: signature_of(node, source),
        }
    }
}

fn do_block<'a, D: Doc>(node: &Node<'a, D>) -> Option<Node<'a, D>> {
    node.children().find(|c| c.kind() == "do_block")
}

/// A `@doc` / `@spec` / … attribute. It annotates the next definition, so it
/// does not end a run of clauses.
fn is_attribute<D: Doc>(node: &Node<'_, D>) -> bool {
    node.kind() == "unary_operator" && node.field("operator").is_some_and(|o| o.text() == "@")
}

/// The Elixir counterpart of the generic `collect_split_points`: same plan,
/// same descent budget, members recognised by macro name.
pub(super) fn collect_split_points<D: Doc>(
    node: &Node<'_, D>,
    source: &str,
    plan: &mut SplitPlan,
    trail: &mut Vec<String>,
    d: Descent,
) {
    let source_len = source.len();
    // The function the previous clause belongs to, and its outline entry.
    let mut last: Option<((String, String, usize), usize)> = None;
    for child in node.children() {
        if !child.is_named() {
            continue;
        }
        let range = child.range();
        let too_big = range.end.saturating_sub(range.start) > MAX_CHARS;
        let form = form_of(&child, source);
        if !matches!(form, Form::Clause { .. })
            && child.kind() != "comment"
            && !is_attribute(&child)
        {
            last = None;
        }

        let member = match form {
            Form::Module { name, signature } => {
                let breadcrumb = join_trail(trail, Some(&name));
                plan.members.push(MemberSpan {
                    start: range.start,
                    end: range.end,
                    breadcrumb,
                    signature,
                });
                if let Some(block) = do_block(&child).filter(|_| !d.exhausted()) {
                    trail.push(name);
                    let next = d.into_child(range.end.saturating_sub(range.start));
                    collect_split_points(&block, source, plan, trail, next);
                    trail.pop();
                }
                true
            }
            Form::Clause {
                key,
                name,
                signature,
            } => {
                let breadcrumb = join_trail(trail, Some(&name));
                match &last {
                    Some((k, idx)) if *k == key => plan.members[*idx].end = range.end,
                    _ => {
                        plan.members.push(MemberSpan {
                            start: range.start,
                            end: range.end,
                            breadcrumb: breadcrumb.clone(),
                            signature,
                        });
                        last = Some((key, plan.members.len() - 1));
                    }
                }
                if too_big {
                    deep_region(&child, source_len, breadcrumb, plan);
                }
                true
            }
            Form::Block { signature } => {
                let breadcrumb = join_trail(trail, None);
                plan.members.push(MemberSpan {
                    start: range.start,
                    end: range.end,
                    breadcrumb: breadcrumb.clone(),
                    signature,
                });
                if too_big {
                    deep_region(&child, source_len, breadcrumb, plan);
                }
                true
            }
            Form::Wrapper => {
                if let Some(block) = do_block(&child).filter(|_| !d.exhausted()) {
                    let next = d.into_child(range.end.saturating_sub(range.start));
                    collect_split_points(&block, source, plan, trail, next);
                }
                false
            }
            Form::Other => false,
        };

        // As in the generic walk: every named child splits at the file root,
        // only members below it.
        if d.ast > 0 && !member {
            continue;
        }
        if range.end > *plan.member_points.last().unwrap() && range.end <= source_len {
            plan.member_points.push(range.end);
        }
    }
}

/// Record an oversized callable's statement boundaries, so it is split into
/// `Part` chunks beside the whole.
fn deep_region<D: Doc>(
    node: &Node<'_, D>,
    source_len: usize,
    breadcrumb: String,
    plan: &mut SplitPlan,
) {
    let range = node.range();
    let mut points = vec![range.start];
    collect_body_points(node, source_len, &mut points);
    if points.last() != Some(&range.end) && range.end <= source_len {
        points.push(range.end);
    }
    if points.len() > 2 {
        plan.deep.push(DeepRegion {
            start: range.start,
            end: range.end,
            breadcrumb,
            points,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn members(src: &str) -> Vec<(String, String)> {
        build_split_plan(src, SupportLang::Elixir)
            .members
            .into_iter()
            .map(|m| (m.breadcrumb, m.signature))
            .collect()
    }

    #[test]
    fn clauses_of_one_function_are_one_outline_entry() {
        let src = "defmodule A do\n  @doc \"f\"\n  def f(0), do: 1\n  @spec f(integer) :: integer\n  def f(n), do: n\n  def f(a, b), do: a + b\n  defp f(x, y, z), do: x\nend\n";
        let got = members(src);
        assert_eq!(
            got,
            vec![
                ("A".to_string(), "defmodule A".to_string()),
                ("A > f".to_string(), "def f(0)".to_string()),
                ("A > f".to_string(), "def f(a, b)".to_string()),
                ("A > f".to_string(), "defp f(x, y, z)".to_string()),
            ]
        );
        let plan = build_split_plan(src, SupportLang::Elixir);
        // The folded entry spans both clauses of f/1.
        let f1 = &plan.members[1];
        assert_eq!(
            &src[f1.start..f1.end],
            "def f(0), do: 1\n  @spec f(integer) :: integer\n  def f(n), do: n"
        );
    }

    #[test]
    fn another_form_between_clauses_ends_the_run() {
        let src = "defmodule A do\n  def f(0), do: 1\n  @x 1\n  def f(n), do: n\nend\n";
        // `@x 1` is an attribute, which annotates; it does not end the run.
        assert_eq!(members(src).len(), 2);
        let src = "defmodule A do\n  def f(0), do: 1\n  plug :x\n  def f(n), do: n\nend\n";
        assert_eq!(members(src).len(), 3);
    }

    #[test]
    fn nested_modules_carry_their_parents_in_the_breadcrumb() {
        let src = "defmodule A do\n  defmodule B.C do\n    def g, do: 1\n  end\nend\n";
        let crumbs: Vec<String> = members(src).into_iter().map(|(b, _)| b).collect();
        assert_eq!(crumbs, vec!["A", "A > B.C", "A > B.C > g"]);
    }

    #[test]
    fn a_wrapper_holding_definitions_is_opened_not_listed() {
        let src = "defmodule A do\n  if true do\n    def g, do: 1\n  end\n  test \"t\" do\n    if x do\n      :ok\n    end\n  end\nend\n";
        let sigs: Vec<String> = members(src).into_iter().map(|(_, s)| s).collect();
        assert_eq!(sigs, vec!["defmodule A", "def g", "test \"t\""]);
    }
}
