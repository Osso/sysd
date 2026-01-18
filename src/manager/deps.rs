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
    /// Alias resolution: symlink name -> canonical name
    aliases: HashMap<String, String>,
}

impl DepGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an alias (symlink name -> canonical name)
    pub fn add_alias(&mut self, alias: &str, canonical: &str) {
        if alias != canonical {
            self.aliases.insert(alias.to_string(), canonical.to_string());
        }
    }

    /// Pre-register a node (unit that was loaded)
    pub fn add_node(&mut self, name: &str) {
        self.nodes.insert(name.to_string());
    }

    /// Resolve a name through aliases to get canonical name
    fn resolve(&self, name: &str) -> String {
        self.aliases.get(name).cloned().unwrap_or_else(|| name.to_string())
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
        // Only add edge if X is a loaded unit
        for dep in &service.unit.before {
            let resolved_dep = self.resolve(dep);
            if self.nodes.contains(&resolved_dep) {
                self.edges
                    .entry(resolved_dep)
                    .or_default()
                    .insert(name.clone());
            }
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
        self.add_unit_with_name(unit.name(), unit);
    }

    /// Add a unit to the graph with an explicit name (for template instances)
    pub fn add_unit_with_name(&mut self, name: &str, unit: &Unit) {
        self.nodes.insert(name.to_string());

        let section = unit.unit_section();

        // DefaultDependencies=yes (default) adds implicit dependencies
        // Different unit types get different default dependencies:
        // - Services: After=basic.target, Before=shutdown.target
        // - Sockets: After=sysinit.target, Before=sockets.target, Before=shutdown.target
        // - Targets: no implicit ordering
        if section.default_dependencies && !unit.is_target() {
            if unit.is_socket() {
                // Sockets use sysinit.target, not basic.target
                // This is crucial because dbus-broker.service has Before=basic.target
                // but After=dbus.socket, so dbus.socket must NOT wait for basic.target
                self.add_edge(name, "sysinit.target");

                // Before=sockets.target - sockets.target depends on us (if loaded)
                if self.nodes.contains("sockets.target") {
                    self.edges
                        .entry("sockets.target".to_string())
                        .or_default()
                        .insert(name.to_string());
                }
            } else {
                // Services and other units wait for basic.target
                self.add_edge(name, "basic.target");
            }

            // Before=shutdown.target - shutdown.target depends on us (if loaded)
            if self.nodes.contains("shutdown.target") {
                self.edges
                    .entry("shutdown.target".to_string())
                    .or_default()
                    .insert(name.to_string());
            }
        }

        // After=X means X must start before us
        for dep in &section.after {
            self.add_edge(name, dep);
        }

        // Before=X means we must start before X
        // Only add edge if X is a loaded unit
        for dep in &section.before {
            let resolved_dep = self.resolve(dep);
            if self.nodes.contains(&resolved_dep) {
                self.edges
                    .entry(resolved_dep)
                    .or_default()
                    .insert(name.to_string());
            }
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
    /// Resolves `to` through aliases to use canonical names
    /// Only creates edge if `to` is already a known node (loaded unit)
    fn add_edge(&mut self, from: &str, to: &str) {
        let resolved_to = self.resolve(to);
        // Only add edge if target exists - After= on missing units is ignored
        if !self.nodes.contains(&resolved_to) {
            return;
        }
        self.edges
            .entry(from.to_string())
            .or_default()
            .insert(resolved_to);
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
        // First collect all transitive dependencies (following graph edges)
        let mut needed: HashSet<String> = HashSet::new();
        let mut to_visit: VecDeque<String> = VecDeque::new();

        // Only add target if it's in the graph
        if self.nodes.contains(target) {
            to_visit.push_back(target.to_string());
            needed.insert(target.to_string());
        }

        while let Some(node) = to_visit.pop_front() {
            if let Some(deps) = self.edges.get(&node) {
                for dep in deps {
                    // Only follow edges to nodes that exist in our graph
                    if self.nodes.contains(dep) && needed.insert(dep.clone()) {
                        to_visit.push_back(dep.clone());
                    }
                }
            }
        }

        // Toposort only the subgraph of needed nodes
        // This avoids cycles from nodes we don't care about (like shutdown.target)
        self.toposort_subset(&needed)
    }

    /// Toposort a subset of the graph, ignoring nodes outside the subset
    /// If cycles exist, break them by adding cycle members in arbitrary order
    fn toposort_subset(&self, subset: &HashSet<String>) -> Result<Vec<String>, CycleError> {
        // Build in-degree map for subset nodes only
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        for node in subset {
            in_degree.insert(node.clone(), 0);
        }

        // Count only edges within the subset
        for (from, deps) in &self.edges {
            if subset.contains(from) {
                let subset_deps = deps.iter().filter(|d| subset.contains(*d)).count();
                *in_degree.entry(from.clone()).or_default() = subset_deps;
            }
        }

        // Start with nodes that have no dependencies (within subset)
        let mut queue: VecDeque<String> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(n, _)| n.clone())
            .collect();

        let mut result = Vec::new();
        let mut added: HashSet<String> = HashSet::new();

        while result.len() < subset.len() {
            if let Some(node) = queue.pop_front() {
                if added.insert(node.clone()) {
                    result.push(node.clone());
                } else {
                    continue; // Skip duplicates
                }

                // For each node in subset that depends on this one, decrement in-degree
                for (dependent, deps) in &self.edges {
                    if subset.contains(dependent) && deps.contains(&node) {
                        if let Some(deg) = in_degree.get_mut(dependent) {
                            *deg = deg.saturating_sub(1);
                            if *deg == 0 {
                                queue.push_back(dependent.clone());
                            }
                        }
                    }
                }
            } else {
                // No nodes with zero in-degree - we have a cycle
                // Break it by picking a node with minimum in-degree from remaining
                let remaining: Vec<_> = in_degree
                    .iter()
                    .filter(|(n, &deg)| deg > 0 && !result.contains(n))
                    .collect();

                if remaining.is_empty() {
                    break;
                }

                // Pick node with minimum in-degree to break cycle
                let (cycle_node, _) = remaining
                    .iter()
                    .min_by_key(|(_, &deg)| deg)
                    .unwrap();

                // Show all cycle participants for debugging
                let cycle_units: Vec<_> = remaining.iter().map(|(n, _)| n.as_str()).collect();
                log::warn!(
                    "Breaking ordering cycle: starting {} early (cycle involves: {})",
                    cycle_node,
                    cycle_units.join(", ")
                );
                eprintln!(
                    "sysd: WARNING: Breaking ordering cycle by starting {} early",
                    cycle_node
                );

                // Add it and reset its in-degree
                queue.push_back(cycle_node.to_string());
                in_degree.insert(cycle_node.to_string(), 0);
            }
        }

        Ok(result)
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
        // Need to pre-register all nodes first, then add edges
        let a = make_service("a.service", &["c.service"]);
        let b = make_service("b.service", &["a.service"]);
        let c = make_service("c.service", &["b.service"]);

        // Pre-register all nodes so edges can be created
        graph.add_node("a.service");
        graph.add_node("b.service");
        graph.add_node("c.service");

        // Now add services (edges will be created since all nodes exist)
        graph.add_service(&a);
        graph.add_service(&b);
        graph.add_service(&c);

        let err = graph.toposort().unwrap_err();
        assert!(!err.nodes.is_empty());
    }

    #[test]
    fn test_before_directive() {
        let mut graph = DepGraph::new();
        // a.Before=b means b depends on a (a starts first)
        let mut a = make_service("a.service", &[]);
        a.unit.before = vec!["b.service".to_string()];

        // Pre-register b.service so Before= edge can be created
        graph.add_node("b.service");
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
