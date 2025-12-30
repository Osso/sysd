//! Dependency resolution for unit ordering
//!
//! Builds a directed acyclic graph from unit dependencies and performs
//! topological sort to determine start order.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::units::{Service, Unit};

/// Dependency graph for ordering service startup
#[derive(Debug, Default)]
pub struct DepGraph {
    /// Edges: node -> nodes that must start BEFORE this node
    /// (i.e., this node is After= those nodes)
    edges: HashMap<String, HashSet<String>>,
    /// All known nodes
    nodes: HashSet<String>,
}

impl DepGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a service to the graph, extracting its dependencies
    pub fn add_service(&mut self, service: &Service) {
        let name = &service.name;
        self.nodes.insert(name.clone());

        // After=X means X must start before us
        // So we have an edge: name depends on X
        for dep in &service.unit.after {
            self.add_edge(name, dep);
        }

        // Before=X means we must start before X
        // So X depends on us: edge from X to name
        for dep in &service.unit.before {
            self.nodes.insert(dep.clone());
            self.edges
                .entry(dep.clone())
                .or_default()
                .insert(name.clone());
        }

        // Requires=X and Wants=X imply After=X for ordering purposes
        // (though Requires also means fail if X fails)
        for dep in &service.unit.requires {
            self.add_edge(name, dep);
        }

        for dep in &service.unit.wants {
            self.add_edge(name, dep);
        }
    }

    /// Add a unit (service or target) to the graph
    pub fn add_unit(&mut self, unit: &Unit) {
        let name = unit.name();
        self.nodes.insert(name.to_string());

        let section = unit.unit_section();

        // DefaultDependencies=yes (default) adds implicit dependencies for services
        // - After=basic.target (service waits for basic system to be up)
        // - Before=shutdown.target (service stops before shutdown)
        // - Conflicts=shutdown.target (stop service when shutdown starts)
        if section.default_dependencies && !unit.is_target() {
            // After=basic.target - we depend on basic.target
            self.add_edge(name, "basic.target");

            // Before=shutdown.target - shutdown.target depends on us
            self.nodes.insert("shutdown.target".to_string());
            self.edges
                .entry("shutdown.target".to_string())
                .or_default()
                .insert(name.to_string());
        }

        // After=X means X must start before us
        for dep in &section.after {
            self.add_edge(name, dep);
        }

        // Before=X means we must start before X
        for dep in &section.before {
            self.nodes.insert(dep.clone());
            self.edges
                .entry(dep.clone())
                .or_default()
                .insert(name.to_string());
        }

        // Requires=X and Wants=X imply ordering dependency
        for dep in &section.requires {
            self.add_edge(name, dep);
        }

        for dep in &section.wants {
            self.add_edge(name, dep);
        }

        // For targets, .wants directory entries are also dependencies
        for dep in unit.wants_dir() {
            self.add_edge(name, dep);
        }
    }

    /// Add a directed edge: `from` depends on `to` (to must start first)
    fn add_edge(&mut self, from: &str, to: &str) {
        self.nodes.insert(from.to_string());
        self.nodes.insert(to.to_string());
        self.edges
            .entry(from.to_string())
            .or_default()
            .insert(to.to_string());
    }

    /// Get direct dependencies of a node (nodes that must start before it)
    pub fn dependencies(&self, name: &str) -> impl Iterator<Item = &String> {
        self.edges.get(name).into_iter().flat_map(|s| s.iter())
    }

    /// Topological sort using Kahn's algorithm
    /// Returns nodes in order they should be started, or an error if cycle detected
    pub fn toposort(&self) -> Result<Vec<String>, CycleError> {
        // Calculate in-degree for each node
        // in_degree[X] = number of dependencies X has (nodes that must start before X)
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        for node in &self.nodes {
            in_degree.insert(node.clone(), 0);
        }

        for (from, deps) in &self.edges {
            // 'from' depends on all nodes in 'deps'
            // This means 'from' can't start until all deps are done
            *in_degree.entry(from.clone()).or_default() = deps.len();
        }

        // Start with nodes that have no dependencies
        let mut queue: VecDeque<String> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(n, _)| n.clone())
            .collect();

        let mut result = Vec::new();

        while let Some(node) = queue.pop_front() {
            result.push(node.clone());

            // For each node that depends on this one, decrement its in-degree
            for (dependent, deps) in &self.edges {
                if deps.contains(&node) {
                    if let Some(deg) = in_degree.get_mut(dependent) {
                        *deg = deg.saturating_sub(1);
                        if *deg == 0 {
                            queue.push_back(dependent.clone());
                        }
                    }
                }
            }
        }

        if result.len() != self.nodes.len() {
            // Find cycle participants
            let remaining: Vec<String> = self
                .nodes
                .iter()
                .filter(|n| !result.contains(n))
                .cloned()
                .collect();
            return Err(CycleError { nodes: remaining });
        }

        Ok(result)
    }

    /// Get the start order for a specific target and its dependencies
    /// Returns only the subset of nodes reachable from the target
    pub fn start_order_for(&self, target: &str) -> Result<Vec<String>, CycleError> {
        // First collect all transitive dependencies
        let mut needed: HashSet<String> = HashSet::new();
        let mut to_visit: VecDeque<String> = VecDeque::new();

        to_visit.push_back(target.to_string());
        needed.insert(target.to_string());

        while let Some(node) = to_visit.pop_front() {
            if let Some(deps) = self.edges.get(&node) {
                for dep in deps {
                    if needed.insert(dep.clone()) {
                        to_visit.push_back(dep.clone());
                    }
                }
            }
        }

        // Now toposort just the needed nodes
        let full_order = self.toposort()?;

        Ok(full_order
            .into_iter()
            .filter(|n| needed.contains(n))
            .collect())
    }
}

/// Error when a dependency cycle is detected
#[derive(Debug, Clone)]
pub struct CycleError {
    pub nodes: Vec<String>,
}

impl std::fmt::Display for CycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Dependency cycle detected involving: {}",
            self.nodes.join(", ")
        )
    }
}

impl std::error::Error for CycleError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::Service;

    fn make_service(name: &str, after: &[&str]) -> Service {
        let mut svc = Service::new(name.to_string());
        svc.unit.after = after.iter().map(|s| s.to_string()).collect();
        svc
    }

    #[test]
    fn test_empty_graph() {
        let graph = DepGraph::new();
        assert_eq!(graph.toposort().unwrap(), Vec::<String>::new());
    }

    #[test]
    fn test_single_node() {
        let mut graph = DepGraph::new();
        graph.add_service(&make_service("a.service", &[]));
        assert_eq!(graph.toposort().unwrap(), vec!["a.service"]);
    }

    #[test]
    fn test_linear_chain() {
        let mut graph = DepGraph::new();
        // c depends on b, b depends on a
        // Start order: a, b, c
        graph.add_service(&make_service("a.service", &[]));
        graph.add_service(&make_service("b.service", &["a.service"]));
        graph.add_service(&make_service("c.service", &["b.service"]));

        let order = graph.toposort().unwrap();
        let a_pos = order.iter().position(|s| s == "a.service").unwrap();
        let b_pos = order.iter().position(|s| s == "b.service").unwrap();
        let c_pos = order.iter().position(|s| s == "c.service").unwrap();

        assert!(a_pos < b_pos);
        assert!(b_pos < c_pos);
    }

    #[test]
    fn test_diamond() {
        let mut graph = DepGraph::new();
        // d depends on b and c
        // b and c both depend on a
        // Valid orders: a, b, c, d or a, c, b, d
        graph.add_service(&make_service("a.service", &[]));
        graph.add_service(&make_service("b.service", &["a.service"]));
        graph.add_service(&make_service("c.service", &["a.service"]));
        graph.add_service(&make_service("d.service", &["b.service", "c.service"]));

        let order = graph.toposort().unwrap();
        let a_pos = order.iter().position(|s| s == "a.service").unwrap();
        let b_pos = order.iter().position(|s| s == "b.service").unwrap();
        let c_pos = order.iter().position(|s| s == "c.service").unwrap();
        let d_pos = order.iter().position(|s| s == "d.service").unwrap();

        assert!(a_pos < b_pos);
        assert!(a_pos < c_pos);
        assert!(b_pos < d_pos);
        assert!(c_pos < d_pos);
    }

    #[test]
    fn test_cycle_detection() {
        let mut graph = DepGraph::new();
        // a -> b -> c -> a (cycle)
        graph.add_service(&make_service("a.service", &["c.service"]));
        graph.add_service(&make_service("b.service", &["a.service"]));
        graph.add_service(&make_service("c.service", &["b.service"]));

        let err = graph.toposort().unwrap_err();
        assert!(!err.nodes.is_empty());
    }

    #[test]
    fn test_before_directive() {
        let mut graph = DepGraph::new();
        // a.Before=b means b depends on a (a starts first)
        let mut a = make_service("a.service", &[]);
        a.unit.before = vec!["b.service".to_string()];
        graph.add_service(&a);
        graph.add_service(&make_service("b.service", &[]));

        let order = graph.toposort().unwrap();
        let a_pos = order.iter().position(|s| s == "a.service").unwrap();
        let b_pos = order.iter().position(|s| s == "b.service").unwrap();
        assert!(a_pos < b_pos);
    }

    #[test]
    fn test_start_order_for_target() {
        let mut graph = DepGraph::new();
        graph.add_service(&make_service("a.service", &[]));
        graph.add_service(&make_service("b.service", &["a.service"]));
        graph.add_service(&make_service("c.service", &["b.service"]));
        graph.add_service(&make_service("unrelated.service", &[]));

        // Getting order for c should only include a, b, c
        let order = graph.start_order_for("c.service").unwrap();
        assert!(order.contains(&"a.service".to_string()));
        assert!(order.contains(&"b.service".to_string()));
        assert!(order.contains(&"c.service".to_string()));
        assert!(!order.contains(&"unrelated.service".to_string()));
    }
}
