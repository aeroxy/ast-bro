//! Iterative Tarjan SCC over a `DepGraph`. Stack-safe via an explicit
//! work stack — no risk of overflowing on huge dep chains.
//!
//! Filters singleton SCCs unless they have a self-edge. Output is
//! deterministic: cycles sorted by member count descending, members
//! sorted within each.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::deps::graph::DepGraph;

#[derive(Debug, Clone)]
pub struct Cycle {
    pub members: Vec<PathBuf>,
}

pub fn detect(graph: &DepGraph, min_size: usize) -> Vec<Cycle> {
    // Compact the file set into u32 indices for tight memory + fast loops.
    let nodes: Vec<PathBuf> = {
        let mut v: Vec<PathBuf> = graph.forward.keys().cloned().collect();
        v.sort();
        v
    };
    let index_of: HashMap<&PathBuf, u32> = nodes
        .iter()
        .enumerate()
        .map(|(i, p)| (p, i as u32))
        .collect();

    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); nodes.len()];
    for (src, edges) in &graph.forward {
        let Some(&si) = index_of.get(src) else {
            continue;
        };
        let mut row = Vec::with_capacity(edges.len());
        for e in edges {
            if let Some(&ti) = index_of.get(&e.target) {
                row.push(ti);
            }
        }
        adj[si as usize] = row;
    }

    // Iterative Tarjan.
    let n = nodes.len();
    let mut state = State::new(n);
    for v in 0..n as u32 {
        if state.index[v as usize].is_none() {
            state.run(v, &adj);
        }
    }

    let mut cycles: Vec<Cycle> = state
        .components
        .into_iter()
        .filter_map(|comp| {
            let len = comp.len();
            if len < min_size {
                if len == 1 {
                    // Keep singletons that self-edge.
                    let v = comp[0];
                    if adj[v as usize].contains(&v) {
                        return Some(Cycle {
                            members: vec![nodes[v as usize].clone()],
                        });
                    }
                }
                return None;
            }
            let mut members: Vec<PathBuf> =
                comp.iter().map(|&i| nodes[i as usize].clone()).collect();
            members.sort();
            Some(Cycle { members })
        })
        .collect();
    cycles.sort_by_key(|b| std::cmp::Reverse(b.members.len()));
    cycles
}

struct State {
    index: Vec<Option<u32>>,
    lowlink: Vec<u32>,
    on_stack: Vec<bool>,
    stack: Vec<u32>,
    next_index: u32,
    components: Vec<Vec<u32>>,
}

enum Frame {
    /// Entered for the first time — push self onto SCC stack and begin
    /// iterating successors.
    Enter { v: u32, child_iter: usize },
    /// Returning from a recursive call into successor `w` of `v`.
    Resume { v: u32, w: u32, child_iter: usize },
}

impl State {
    fn new(n: usize) -> Self {
        Self {
            index: vec![None; n],
            lowlink: vec![0; n],
            on_stack: vec![false; n],
            stack: Vec::new(),
            next_index: 0,
            components: Vec::new(),
        }
    }

    fn run(&mut self, root: u32, adj: &[Vec<u32>]) {
        let mut work: Vec<Frame> = Vec::new();
        work.push(Frame::Enter {
            v: root,
            child_iter: 0,
        });

        while let Some(frame) = work.pop() {
            match frame {
                Frame::Enter { v, child_iter } => {
                    if self.index[v as usize].is_none() {
                        self.index[v as usize] = Some(self.next_index);
                        self.lowlink[v as usize] = self.next_index;
                        self.next_index += 1;
                        self.stack.push(v);
                        self.on_stack[v as usize] = true;
                    }
                    self.advance(v, child_iter, adj, &mut work);
                }
                Frame::Resume { v, w, child_iter } => {
                    let lw = self.lowlink[w as usize];
                    let lv = self.lowlink[v as usize];
                    if lw < lv {
                        self.lowlink[v as usize] = lw;
                    }
                    self.advance(v, child_iter, adj, &mut work);
                }
            }
        }
    }

    fn advance(&mut self, v: u32, mut idx: usize, adj: &[Vec<u32>], work: &mut Vec<Frame>) {
        let neighbours = &adj[v as usize];
        while idx < neighbours.len() {
            let w = neighbours[idx];
            if self.index[w as usize].is_none() {
                // Recurse into w; come back to (v, w, idx+1) after.
                work.push(Frame::Resume {
                    v,
                    w,
                    child_iter: idx + 1,
                });
                work.push(Frame::Enter {
                    v: w,
                    child_iter: 0,
                });
                return;
            } else if self.on_stack[w as usize] {
                let widx = self.index[w as usize].unwrap();
                if widx < self.lowlink[v as usize] {
                    self.lowlink[v as usize] = widx;
                }
            }
            idx += 1;
        }
        // Finished with v's successors — check if it's an SCC root.
        if self.lowlink[v as usize] == self.index[v as usize].unwrap() {
            let mut comp = Vec::new();
            while let Some(w) = self.stack.pop() {
                self.on_stack[w as usize] = false;
                comp.push(w);
                if w == v {
                    break;
                }
            }
            self.components.push(comp);
        }
    }
}
