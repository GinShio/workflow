//! The machete forest: the dependency tree, and the tree algebra over it.
//!
//! `.git/machete` is the one piece of state the stack tool reads to know how
//! branches relate — who sits on whom. It is a forest, not just a chain: a branch
//! can fork into several. This module owns parsing that file, writing it back,
//! and the handful of tree queries the verbs need. It is deliberately free of any
//! git or network contact, because the tree rules are fiddly enough that being
//! able to test them on a literal string is worth a lot.
//!
//! The representation is name-keyed maps rather than pointer-linked nodes. A
//! tree of owned nodes in Rust fights the borrow checker for no real gain here;
//! the forest is small and every query naturally produces a list of branch
//! names, so storing relationships as `name → data` keeps the algebra to plain
//! lookups and the queries to simple recursion.

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
struct Node {
    parent: Option<String>,
    children: Vec<String>,
    annotation: String,
}

#[derive(Debug, Clone, Default)]
pub struct Topology {
    nodes: HashMap<String, Node>,
    /// Names in the order they were first seen, so serialization and `--all`
    /// traversal are stable and match what the user wrote.
    order: Vec<String>,
}

impl Topology {
    /// Parse the indentation-encoded forest. A line's parent is the nearest
    /// preceding line with strictly smaller indent, which is what lets the same
    /// file express both linear chains and forks without any explicit syntax.
    pub fn parse(text: &str) -> Self {
        let mut topo = Topology::default();
        // The deepest node seen at each indent level so far; a new line attaches
        // to the closest shallower one. Levels deeper than the current line are
        // stale once we step back out, so they get dropped.
        let mut last_at_indent: Vec<(usize, String)> = Vec::new();

        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let indent = line.chars().take_while(|c| c.is_whitespace()).count();
            let mut parts = line.trim().splitn(2, char::is_whitespace);
            let name = parts.next().unwrap().to_owned();
            let annotation = parts.next().unwrap_or("").trim().to_owned();

            last_at_indent.retain(|(i, _)| *i < indent);
            let parent = last_at_indent.last().map(|(_, n)| n.clone());

            if let Some(p) = &parent {
                if let Some(pn) = topo.nodes.get_mut(p) {
                    pn.children.push(name.clone());
                }
            }
            topo.nodes.insert(
                name.clone(),
                Node {
                    parent,
                    children: Vec::new(),
                    annotation,
                },
            );
            topo.order.push(name.clone());
            last_at_indent.push((indent, name));
        }

        topo
    }

    /// Render back to the on-disk format, four spaces per level, traversing each
    /// root depth-first. Reading is indent-agnostic (only relative nesting
    /// matters), so a file written by hand or by `git-machete` with a different
    /// unit still parses; we just normalize to one width on the way out.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for root in self.roots() {
            self.render_node(&root, 0, &mut out);
        }
        out
    }

    fn render_node(&self, name: &str, depth: usize, out: &mut String) {
        let node = &self.nodes[name];
        out.push_str(&"    ".repeat(depth));
        out.push_str(name);
        if !node.annotation.is_empty() {
            out.push(' ');
            out.push_str(&node.annotation);
        }
        out.push('\n');
        for child in &node.children {
            self.render_node(child, depth + 1, out);
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.nodes.contains_key(name)
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn parent(&self, name: &str) -> Option<&str> {
        self.nodes.get(name).and_then(|n| n.parent.as_deref())
    }

    pub fn children(&self, name: &str) -> &[String] {
        self.nodes
            .get(name)
            .map(|n| n.children.as_slice())
            .unwrap_or(&[])
    }

    pub fn annotation(&self, name: &str) -> Option<&str> {
        self.nodes.get(name).map(|n| n.annotation.as_str())
    }

    pub fn set_annotation(&mut self, name: &str, annotation: impl Into<String>) {
        if let Some(node) = self.nodes.get_mut(name) {
            node.annotation = annotation.into();
        }
    }

    /// Every node, in file order — the universe `--all` operates on.
    pub fn all(&self) -> &[String] {
        &self.order
    }

    /// Roots (no parent), in file order.
    pub fn roots(&self) -> Vec<String> {
        self.order
            .iter()
            .filter(|n| self.nodes[*n].parent.is_none())
            .cloned()
            .collect()
    }

    /// A node has a fork below it when it carries more than one child; this is
    /// the threshold that switches the default scope from "one line of work" to
    /// "the whole tree I manage".
    pub fn is_fork_point(&self, name: &str) -> bool {
        self.children(name).len() >= 2
    }

    /// Root → … → parent, excluding the node itself.
    pub fn ancestors(&self, name: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut cur = self.parent(name).map(str::to_owned);
        while let Some(p) = cur {
            cur = self.parent(&p).map(str::to_owned);
            chain.push(p);
        }
        chain.reverse();
        chain
    }

    /// The node and all descendants, DFS pre-order.
    pub fn subtree(&self, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_subtree(name, &mut out);
        out
    }

    fn collect_subtree(&self, name: &str, out: &mut Vec<String>) {
        out.push(name.to_owned());
        for child in self.children(name) {
            self.collect_subtree(child, out);
        }
    }

    /// Ancestors + the node + the primary (first-child) chain down to a leaf.
    /// This is "one line of work": it deliberately ignores sibling branches,
    /// which belong to a different line the user isn't standing on.
    pub fn linear_stack(&self, name: &str) -> Vec<String> {
        let mut out = self.ancestors(name);
        out.push(name.to_owned());
        let mut cur = name.to_owned();
        while let Some(first) = self.children(&cur).first() {
            out.push(first.clone());
            cur = first.clone();
        }
        out
    }

    /// The navigation chains to render in a node's MR description (§8 of the
    /// design). A fork-point yields one chain per child; a linear node yields
    /// one; a leaf yields just its own lineage. Each downstream walk stops at the
    /// next fork-point, because that fork renders its own multi-chain block and
    /// repeating its subtree here would explode the description.
    pub fn anno_blocks(&self, name: &str) -> Vec<Vec<String>> {
        let mut prefix = self.ancestors(name);
        prefix.push(name.to_owned());

        let children = self.children(name);
        match children.len() {
            0 => vec![prefix],
            1 => {
                let mut block = prefix;
                block.extend(self.path_to_next_fork_or_leaf(&children[0]));
                vec![block]
            }
            _ => children
                .iter()
                .map(|child| {
                    let mut block = prefix.clone();
                    block.extend(self.path_to_next_fork_or_leaf(child));
                    block
                })
                .collect(),
        }
    }

    /// Walk from `start` following the single child at each step, stopping
    /// (inclusive) at the first leaf or fork-point.
    fn path_to_next_fork_or_leaf(&self, start: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = start.to_owned();
        loop {
            out.push(cur.clone());
            let children = self.children(&cur);
            if children.len() != 1 {
                break;
            }
            cur = children[0].clone();
        }
        out
    }

    /// Insert a bare node if it isn't present. `slice` uses this before linking
    /// so it can lay a chain over a file that may already mention some of the
    /// branches (and may not mention the base at all).
    pub fn ensure(&mut self, name: &str) {
        if !self.nodes.contains_key(name) {
            self.nodes.insert(name.to_owned(), Node::default());
            self.order.push(name.to_owned());
        }
    }

    /// Move `child` under `parent`, detaching it from any previous parent. Both
    /// must already exist. This is how `slice` writes the chain it discovered
    /// without disturbing unrelated stacks already in the file.
    ///
    /// The link is refused if it would form a cycle — i.e. `parent` is `child`
    /// itself or already sits below it. This matters because `slice` feeds in
    /// names the user typed (and slug collisions can repeat one), and a cycle
    /// would make every later tree walk recurse forever. Parsing can't produce a
    /// cycle (indentation yields a forest), so guarding here closes the only door.
    /// Returns `false` (changing nothing) when the link is refused as a cycle,
    /// so callers like `mv` can report it rather than silently no-op. Note the
    /// child keeps its own children, so moving a node moves its whole subtree.
    pub fn reparent(&mut self, child: &str, parent: &str) -> bool {
        let mut ancestor = Some(parent.to_owned());
        while let Some(name) = ancestor {
            if name == child {
                return false;
            }
            ancestor = self.parent(&name).map(str::to_owned);
        }

        // Already correctly placed: do nothing. Re-slicing a line that forks
        // would otherwise detach and re-append the child, silently reordering the
        // fork's siblings; this keeps the operation idempotent and order-stable.
        if self.parent(child) == Some(parent) {
            return true;
        }

        if let Some(old) = self.nodes.get(child).and_then(|n| n.parent.clone()) {
            if let Some(old_node) = self.nodes.get_mut(&old) {
                old_node.children.retain(|c| c != child);
            }
        }
        if let Some(child_node) = self.nodes.get_mut(child) {
            child_node.parent = Some(parent.to_owned());
        }
        if let Some(parent_node) = self.nodes.get_mut(parent) {
            if !parent_node.children.iter().any(|c| c == child) {
                parent_node.children.push(child.to_owned());
            }
        }
        true
    }

    /// Remove a node, splicing its children up into the slot it occupied under
    /// its parent. The subtree below the node is **preserved** — re-parented, not
    /// discarded — because removing one branch from a stack must never destroy
    /// the line of work built on top of it. A removed root's children become
    /// roots. Returns whether a node was actually removed.
    pub fn remove(&mut self, name: &str) -> bool {
        let Some(node) = self.nodes.remove(name) else {
            return false;
        };
        let parent = node.parent;
        let children = node.children;

        for child in &children {
            if let Some(child_node) = self.nodes.get_mut(child) {
                child_node.parent = parent.clone();
            }
        }
        if let Some(parent) = &parent {
            if let Some(parent_node) = self.nodes.get_mut(parent) {
                match parent_node.children.iter().position(|c| c == name) {
                    Some(pos) => {
                        parent_node.children.splice(pos..=pos, children);
                    }
                    None => parent_node.children.extend(children),
                }
            }
        }
        self.order.retain(|n| n != name);
        true
    }

    /// Build a trivial `base → branch` forest for a branch that isn't recorded in
    /// the file — the single-node-stack path that lets an ordinary one-off MR
    /// work with zero machete setup.
    pub fn synthetic(base: &str, branch: &str) -> Self {
        let mut topo = Topology::default();
        topo.nodes.insert(
            base.to_owned(),
            Node {
                parent: None,
                children: vec![branch.to_owned()],
                annotation: String::new(),
            },
        );
        topo.nodes.insert(
            branch.to_owned(),
            Node {
                parent: Some(base.to_owned()),
                children: Vec::new(),
                annotation: String::new(),
            },
        );
        topo.order = vec![base.to_owned(), branch.to_owned()];
        topo
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The sample forest from the design's behaviour notes:
    //   main → A → B(fork) → C(fork) → E
    //                                → G
    //                      → D → F
    fn sample() -> Topology {
        Topology::parse(
            "main\n    A\n        B\n            C\n                E\n                G\n            D\n                F\n",
        )
    }

    #[test]
    fn parse_records_parents_and_children() {
        let t = sample();
        assert_eq!(t.parent("A"), Some("main"));
        assert_eq!(t.parent("E"), Some("C"));
        assert_eq!(t.children("B"), ["C".to_owned(), "D".to_owned()]);
        assert!(t.is_fork_point("B"));
        assert!(!t.is_fork_point("A"));
    }

    #[test]
    fn ancestors_and_subtree() {
        let t = sample();
        assert_eq!(t.ancestors("E"), ["main", "A", "B", "C"]);
        assert_eq!(t.subtree("B"), ["B", "C", "E", "G", "D", "F"]);
    }

    #[test]
    fn linear_stack_follows_first_child() {
        let t = sample();
        // From D: ancestors main,A,B + D + first-child chain F.
        assert_eq!(t.linear_stack("D"), ["main", "A", "B", "D", "F"]);
    }

    #[test]
    fn anno_blocks_split_at_forks() {
        let t = sample();
        // B is a fork: one block per child, each stopping at the next fork (C)
        // or leaf (F).
        let blocks = t.anno_blocks("B");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], ["main", "A", "B", "C"]);
        assert_eq!(blocks[1], ["main", "A", "B", "D", "F"]);
        // A is linear into the fork B, so its single block stops at B.
        assert_eq!(t.anno_blocks("A"), vec![vec!["main", "A", "B"]]);
    }

    #[test]
    fn round_trips_through_render() {
        let text = "main\n    A\n        B\n    C\n";
        let t = Topology::parse(text);
        assert_eq!(t.render(), text);
    }

    #[test]
    fn reads_any_indent_and_writes_four_spaces() {
        // Two-space and tab inputs parse the same and normalize to four spaces.
        let four = "main\n    A\n        B\n";
        assert_eq!(Topology::parse("main\n  A\n    B\n").render(), four);
        assert_eq!(Topology::parse("main\n\tA\n\t\tB\n").render(), four);
    }

    #[test]
    fn annotation_is_parsed_and_rewritten() {
        let mut t = Topology::parse("main\n    feature PR #7\n");
        assert_eq!(t.annotation("feature"), Some("PR #7"));
        t.set_annotation("feature", "PR #8");
        assert_eq!(t.render(), "main\n    feature PR #8\n");
    }

    #[test]
    fn remove_splices_children_up_and_preserves_the_subtree() {
        // main → A → B → C, plus a sibling X under A. Removing B must keep C
        // (reattached to A in B's slot), never discard it.
        let mut t = Topology::parse("main\n    A\n        B\n            C\n        X\n");
        assert!(t.remove("B"));
        assert_eq!(t.parent("C"), Some("A"));
        // C lands where B was: before X, preserving the primary line order.
        assert_eq!(t.children("A"), ["C".to_owned(), "X".to_owned()]);
        assert!(!t.contains("B"));
    }

    #[test]
    fn removing_a_root_makes_its_children_roots() {
        let mut t = Topology::parse("main\n    A\n        B\n");
        assert!(t.remove("main"));
        assert_eq!(t.parent("A"), None);
        assert_eq!(t.roots(), ["A".to_owned()]);
    }

    #[test]
    fn synthetic_is_a_two_node_chain() {
        let t = Topology::synthetic("main", "fix");
        assert_eq!(t.parent("fix"), Some("main"));
        assert_eq!(t.children("main"), ["fix".to_owned()]);
    }

    #[test]
    fn reparent_appends_a_sibling_to_build_a_fork() {
        // The multi-round slice case: A already has child B; a later round adds D
        // under A, turning A into a fork rather than replacing B.
        let mut t = Topology::parse("main\n    A\n        B\n");
        t.ensure("D");
        t.reparent("D", "A");
        assert_eq!(t.children("A"), ["B".to_owned(), "D".to_owned()]);
        assert!(t.is_fork_point("A"));
    }

    #[test]
    fn deleting_a_middle_line_reattaches_the_child_to_the_grandparent() {
        // The recommended way to drop a middle branch is to delete its line in
        // .git/machete. Parsing is indent-agnostic, so the orphaned child does
        // not even need re-indenting: C, left at its old depth with B gone,
        // still attaches to A (the nearest shallower line).
        let t = Topology::parse("main\n    A\n            C\n");
        assert_eq!(t.parent("C"), Some("A"));
        assert_eq!(t.children("A"), ["C".to_owned()]);
    }

    #[test]
    fn reparent_to_current_parent_is_an_order_preserving_noop() {
        // A forks to [C, D]; re-asserting C under A (as re-slicing does) must not
        // reorder the siblings.
        let mut t = Topology::parse("main\n    A\n        C\n        D\n");
        assert!(t.reparent("C", "A"));
        assert_eq!(t.children("A"), ["C".to_owned(), "D".to_owned()]);
    }

    #[test]
    fn reparent_refuses_cycles() {
        let mut t = Topology::parse("main\n    A\n        B\n");
        // Self-link (e.g. a slug collision repeating a name) is rejected.
        t.reparent("A", "A");
        assert_eq!(t.parent("A"), Some("main"));
        // Linking an ancestor under its descendant is rejected too.
        t.reparent("main", "B");
        assert_eq!(t.parent("main"), None);
        // And render must terminate (no cycle was introduced).
        assert_eq!(t.render(), "main\n    A\n        B\n");
    }
}
