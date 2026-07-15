use std::{
    collections::{HashMap, HashSet},
    fs, io,
    path::Path,
};

use crate::{
    commands::read_symbol::{normalize_path, symbol_files},
    symbols::{self, Symbol, SymbolKind, SymbolLanguage},
};

pub type NodeId = usize;

/// A symbol definition, plus its resolved and unresolved outgoing call edges.
#[derive(Debug)]
pub struct SymbolNode {
    #[allow(dead_code)]
    pub id: NodeId,
    pub kind: SymbolKind,
    pub name: String,
    pub path: String,
    pub symbol: Symbol,
    pub calls: Vec<NodeId>,
    pub unresolved_calls: Vec<String>,
}

/// An off-site symbol reached by traversing call edges from a failure site,
/// ready to be offered to the relevance ranker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub symbol: String,
    pub path: String,
    pub start_line: usize,
    pub code: String,
}

/// In-memory graph of symbol definitions (nodes) and resolved call edges,
/// built once per `select_context` step. Not persisted — the agent mutates
/// files during a run, so a rebuild avoids staleness.
#[derive(Debug)]
pub struct CodeGraph {
    nodes: Vec<SymbolNode>,
    by_name: HashMap<String, Vec<NodeId>>,
    by_path: HashMap<String, Vec<NodeId>>,
    sources: HashMap<String, String>,
}

impl CodeGraph {
    pub fn build(root: &Path) -> io::Result<CodeGraph> {
        let mut nodes: Vec<SymbolNode> = Vec::new();
        let mut by_name: HashMap<String, Vec<NodeId>> = HashMap::new();
        let mut by_path: HashMap<String, Vec<NodeId>> = HashMap::new();
        let mut sources: HashMap<String, String> = HashMap::new();

        for (file_path, language) in symbol_files(root) {
            let relative_path = normalize_path(file_path.strip_prefix(root).unwrap_or(&file_path));
            let source = fs::read_to_string(&file_path)?;
            let file_symbols = symbols::parse_symbols(&source, language)?;

            for symbol in file_symbols {
                let id = nodes.len();
                by_path.entry(relative_path.clone()).or_default().push(id);
                if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method) {
                    by_name.entry(symbol.name.clone()).or_default().push(id);
                }
                nodes.push(SymbolNode {
                    id,
                    kind: symbol.kind.clone(),
                    name: symbol.name.clone(),
                    path: relative_path.clone(),
                    symbol,
                    calls: Vec::new(),
                    unresolved_calls: Vec::new(),
                });
            }

            sources.insert(relative_path, source);
        }

        let mut graph = CodeGraph {
            nodes,
            by_name,
            by_path,
            sources,
        };
        graph.resolve_calls()?;

        Ok(graph)
    }

    /// Find the innermost node whose span contains `line` in `path`.
    pub fn enclosing(&self, path: &str, line: usize) -> Option<NodeId> {
        let ids = self.by_path.get(path)?;

        ids.iter()
            .copied()
            .filter(|id| {
                let symbol = &self.nodes[*id].symbol;
                symbol.start_line <= line && line <= symbol.end_line
            })
            .min_by_key(|id| self.nodes[*id].symbol.end_line - self.nodes[*id].symbol.start_line)
    }

    /// Walk call edges from the node enclosing `(path, line)`: a same-file
    /// callee recurses into that callee's own calls; a cross-file callee is
    /// emitted as a candidate. Bounded by `max_candidates` and `max_depth`.
    pub fn callees_from(
        &self,
        path: &str,
        line: usize,
        max_candidates: usize,
        max_depth: usize,
    ) -> Vec<Candidate> {
        let Some(start) = self.enclosing(path, line) else {
            return Vec::new();
        };

        let mut visited = HashSet::new();
        visited.insert(start);
        let mut candidates = Vec::new();
        self.walk_callees(
            start,
            0,
            max_candidates,
            max_depth,
            &mut visited,
            &mut candidates,
        );

        candidates
    }

    fn walk_callees(
        &self,
        node_id: NodeId,
        depth: usize,
        max_candidates: usize,
        max_depth: usize,
        visited: &mut HashSet<NodeId>,
        candidates: &mut Vec<Candidate>,
    ) {
        if depth >= max_depth {
            return;
        }

        let node = &self.nodes[node_id];
        for &callee_id in &node.calls {
            if candidates.len() >= max_candidates {
                return;
            }
            if !visited.insert(callee_id) {
                continue;
            }

            let callee = &self.nodes[callee_id];
            if callee.path == node.path {
                self.walk_callees(
                    callee_id,
                    depth + 1,
                    max_candidates,
                    max_depth,
                    visited,
                    candidates,
                );
            } else if let Some(code) = self.slice(
                &callee.path,
                callee.symbol.start_byte,
                callee.symbol.end_byte,
            ) {
                candidates.push(Candidate {
                    symbol: callee.name.clone(),
                    path: callee.path.clone(),
                    start_line: callee.symbol.start_line,
                    code,
                });
            }
        }
    }

    fn slice(&self, path: &str, start_byte: usize, end_byte: usize) -> Option<String> {
        self.sources
            .get(path)?
            .get(start_byte..end_byte)
            .map(str::to_string)
    }

    /// Extract + resolve call edges for every Function/Method node. Calls are
    /// only computed for Function/Method nodes (never for the enclosing
    /// Impl/Class node), so a call inside a Rust `fn` lands on that `fn`, not
    /// on the enclosing `impl` block.
    fn resolve_calls(&mut self) -> io::Result<()> {
        for id in 0..self.nodes.len() {
            let (path, kind, start_byte, end_byte) = {
                let node = &self.nodes[id];
                (
                    node.path.clone(),
                    node.kind.clone(),
                    node.symbol.start_byte,
                    node.symbol.end_byte,
                )
            };
            if !matches!(kind, SymbolKind::Function | SymbolKind::Method) {
                continue;
            }
            let Some(language) = SymbolLanguage::from_path(Path::new(&path)) else {
                continue;
            };
            let Some(source) = self.sources.get(&path) else {
                continue;
            };

            let call_names = symbols::parse_calls(source, language, start_byte, end_byte)?;

            let mut resolved = Vec::new();
            let mut unresolved = Vec::new();
            for name in call_names {
                match self.resolve_call(&name, &path) {
                    Some(target_id) if target_id != id => resolved.push(target_id),
                    Some(_) => {}
                    None => unresolved.push(name),
                }
            }

            self.nodes[id].calls = resolved;
            self.nodes[id].unresolved_calls = unresolved;
        }

        Ok(())
    }

    /// Resolve a callee name to a node: prefer a definition in the same file;
    /// else a unique repo-wide definition; else leave unresolved (mirrors
    /// `read_symbol`'s ambiguity policy — the planner can still reach it via
    /// `search`/`sym`).
    fn resolve_call(&self, name: &str, path: &str) -> Option<NodeId> {
        let candidates = self.by_name.get(name)?;
        let same_file: Vec<NodeId> = candidates
            .iter()
            .copied()
            .filter(|id| self.nodes[*id].path == path)
            .collect();

        if same_file.len() == 1 {
            return Some(same_file[0]);
        }
        if same_file.is_empty() && candidates.len() == 1 {
            return Some(candidates[0]);
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn temp_repo_root(label: &str) -> std::path::PathBuf {
        env::temp_dir().join(format!(
            "haycut-code-graph-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock should be after Unix epoch")
                .as_nanos()
        ))
    }

    #[test]
    fn builds_nodes_and_resolves_cross_file_call_edges() {
        let root = temp_repo_root("nodes-and-edges");
        let src = root.join("src");
        fs::create_dir_all(&src).expect("src should be created");
        fs::write(
            src.join("cart.rs"),
            "fn total_for() -> i64 {\n    apply_bulk_discount()\n}\n",
        )
        .expect("cart.rs should be written");
        fs::write(
            src.join("pricing.rs"),
            "pub fn apply_bulk_discount() -> i64 {\n    100\n}\n",
        )
        .expect("pricing.rs should be written");

        let graph = CodeGraph::build(&root).expect("graph should build");

        let total_for = graph
            .nodes
            .iter()
            .find(|node| node.name == "total_for")
            .expect("total_for node should exist");
        let apply_bulk_discount_id = graph
            .nodes
            .iter()
            .find(|node| node.name == "apply_bulk_discount")
            .expect("apply_bulk_discount node should exist")
            .id;

        assert_eq!(total_for.calls, vec![apply_bulk_discount_id]);
        assert!(total_for.unresolved_calls.is_empty());

        fs::remove_dir_all(root).expect("test repo should be removed");
    }

    #[test]
    fn callees_from_ignores_distractors_on_sibling_call_paths() {
        let root = temp_repo_root("distractors");
        let src = root.join("src");
        fs::create_dir_all(&src).expect("src should be created");
        fs::write(
            src.join("cart.rs"),
            "fn total_for() -> i64 {\n    apply_bulk_discount()\n}\n\n\
             fn describe_order() -> String {\n    log_order();\n    format_receipt_line()\n}\n",
        )
        .expect("cart.rs should be written");
        fs::write(
            src.join("pricing.rs"),
            "pub fn apply_bulk_discount() -> i64 {\n    100\n}\n",
        )
        .expect("pricing.rs should be written");
        fs::write(
            src.join("logging.rs"),
            "pub fn log_order() -> String {\n    String::new()\n}\n",
        )
        .expect("logging.rs should be written");
        fs::write(
            src.join("receipt.rs"),
            "pub fn format_receipt_line() -> String {\n    String::new()\n}\n",
        )
        .expect("receipt.rs should be written");

        let graph = CodeGraph::build(&root).expect("graph should build");

        let candidates = graph.callees_from("src/cart.rs", 2, 5, 3);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].symbol, "apply_bulk_discount");
        assert_eq!(candidates[0].path, "src/pricing.rs");

        fs::remove_dir_all(root).expect("test repo should be removed");
    }

    #[test]
    fn callees_from_finds_call_inside_assert_macro() {
        let root = temp_repo_root("assert-macro");
        let src = root.join("src");
        fs::create_dir_all(&src).expect("src should be created");
        fs::write(
            src.join("cart.rs"),
            "fn total_for(quantity: u32, unit_price_cents: i64) -> i64 {\n    \
             apply_bulk_discount(quantity, unit_price_cents)\n}\n\n\
             fn ten_units_qualifies_for_bulk_discount() {\n    \
             assert_eq!(total_for(10, 100), 900);\n}\n",
        )
        .expect("cart.rs should be written");
        fs::write(
            src.join("pricing.rs"),
            "pub fn apply_bulk_discount(quantity: u32, unit_price_cents: i64) -> i64 {\n    \
             quantity as i64 * unit_price_cents\n}\n",
        )
        .expect("pricing.rs should be written");

        let graph = CodeGraph::build(&root).expect("graph should build");

        let candidates = graph.callees_from("src/cart.rs", 6, 5, 3);

        assert_eq!(
            candidates.len(),
            1,
            "expected the macro-token fallback to reach `total_for` and then \
             `apply_bulk_discount`, got {candidates:?}"
        );
        assert_eq!(candidates[0].symbol, "apply_bulk_discount");
        assert_eq!(candidates[0].path, "src/pricing.rs");

        fs::remove_dir_all(root).expect("test repo should be removed");
    }
}
