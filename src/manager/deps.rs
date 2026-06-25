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
            self.aliases
                .insert(alias.to_string(), canonical.to_string());
        }
    }

    /// Pre-register a node (unit that was loaded)
    pub fn add_node(&mut self, name: &str) {
        self.nodes.insert(name.to_string());
    }

    /// Resolve a name through aliases to get canonical name
    fn resolve(&self, name: &str) -> String {
        self.aliases
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
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
        for dep in &service.unit.before {
            self.add_reverse_edge(name, dep);
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

        if section.default_dependencies && !unit.is_target() {
            self.add_default_dependencies(name, unit);
        }

        for dep in &section.after {
            self.add_edge(name, dep);
        }
        for dep in &section.before {
            self.add_reverse_edge(name, dep);
        }
        for dep in &section.requires {
            self.add_edge(name, dep);
        }
        for dep in &section.wants {
            self.add_edge(name, dep);
        }
        for dep in unit.wants_dir() {
            self.add_edge(name, dep);
        }
    }

    /// Add implicit ordering dependencies based on unit type
    fn add_default_dependencies(&mut self, name: &str, unit: &Unit) {
        if unit.is_socket() {
            self.add_edge(name, "sysinit.target");
            self.add_reverse_edge(name, "sockets.target");
        } else {
            self.add_edge(name, "basic.target");
        }
        self.add_reverse_edge(name, "shutdown.target");
    }

    /// Add a directed edge: `from` depends on `to` (to must start first)
    /// Only creates edge if `to` is already a known node (loaded unit)
    fn add_edge(&mut self, from: &str, to: &str) {
        let resolved_to = self.resolve(to);
        if !self.nodes.contains(&resolved_to) {
            return;
        }
        self.edges
            .entry(from.to_string())
            .or_default()
            .insert(resolved_to);
    }

    /// Add a reverse edge: `dependent` must start before `target`
    /// (i.e., Before=target means target depends on us)
    fn add_reverse_edge(&mut self, dependent: &str, target: &str) {
        let resolved = self.resolve(target);
        if self.nodes.contains(&resolved) {
            self.edges
                .entry(resolved)
                .or_default()
                .insert(dependent.to_string());
        }
    }

    /// Get direct dependencies of a node (nodes that must start before it)
    pub fn dependencies(&self, name: &str) -> impl Iterator<Item = &String> {
        self.edges.get(name).into_iter().flat_map(|s| s.iter())
    }

    /// Topological sort using Kahn's algorithm
    pub fn toposort(&self) -> Result<Vec<String>, CycleError> {
        let mut in_degree = self.compute_in_degree(&self.nodes);
        let mut result = Vec::new();

        kahn_drain(&self.edges, &mut in_degree, &mut result);

        if result.len() != self.nodes.len() {
            let remaining = self
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
    pub fn start_order_for(&self, target: &str) -> Result<Vec<String>, CycleError> {
        let needed = self.transitive_deps(target);
        self.toposort_subset(&needed)
    }

    /// Collect all transitive dependencies reachable from a node
    fn transitive_deps(&self, target: &str) -> HashSet<String> {
        let mut needed = HashSet::new();
        let mut to_visit = VecDeque::new();

        if self.nodes.contains(target) {
            to_visit.push_back(target.to_string());
            needed.insert(target.to_string());
        }

        while let Some(node) = to_visit.pop_front() {
            if let Some(deps) = self.edges.get(&node) {
                for dep in deps {
                    if self.nodes.contains(dep) && needed.insert(dep.clone()) {
                        to_visit.push_back(dep.clone());
                    }
                }
            }
        }

        needed
    }

    /// Toposort a subset of the graph, breaking cycles if needed
    fn toposort_subset(&self, subset: &HashSet<String>) -> Result<Vec<String>, CycleError> {
        let mut in_degree = self.compute_in_degree(subset);
        let mut result = Vec::new();

        loop {
            let before = result.len();
            kahn_drain(&self.edges, &mut in_degree, &mut result);

            if result.len() >= subset.len() {
                break;
            }
            if result.len() == before && !break_cycle(&mut in_degree, &result) {
                break;
            }
        }

        Ok(result)
    }

    /// Build in-degree map for a set of nodes, counting only edges within the set
    fn compute_in_degree(&self, nodes: &HashSet<String>) -> HashMap<String, usize> {
        let mut in_degree: HashMap<String, usize> = nodes.iter().map(|n| (n.clone(), 0)).collect();

        for (from, deps) in &self.edges {
            if nodes.contains(from) {
                let count = deps.iter().filter(|d| nodes.contains(*d)).count();
                *in_degree.entry(from.clone()).or_default() = count;
            }
        }

        in_degree
    }
}

/// Run Kahn's BFS: pop zero-in-degree nodes, decrement dependents
fn kahn_drain(
    edges: &HashMap<String, HashSet<String>>,
    in_degree: &mut HashMap<String, usize>,
    result: &mut Vec<String>,
) {
    let mut emitted: HashSet<String> = result.iter().cloned().collect();
    let mut queued: HashSet<String> = HashSet::new();
    let mut queue = initial_zero_in_degree_queue(in_degree, &emitted, &mut queued);

    while let Some(node) = queue.pop_front() {
        queued.remove(&node);
        if !emitted.insert(node.clone()) {
            continue;
        }
        result.push(node.clone());
        queue_newly_unblocked_nodes(edges, in_degree, &node, &emitted, &mut queued, &mut queue);
    }
}

fn initial_zero_in_degree_queue(
    in_degree: &HashMap<String, usize>,
    emitted: &HashSet<String>,
    queued: &mut HashSet<String>,
) -> VecDeque<String> {
    let mut queue = VecDeque::new();
    for (node, &degree) in in_degree {
        if degree != 0 || emitted.contains(node) {
            continue;
        }
        if queued.insert(node.clone()) {
            queue.push_back(node.clone());
        }
    }
    queue
}

fn queue_newly_unblocked_nodes(
    edges: &HashMap<String, HashSet<String>>,
    in_degree: &mut HashMap<String, usize>,
    node: &str,
    emitted: &HashSet<String>,
    queued: &mut HashSet<String>,
    queue: &mut VecDeque<String>,
) {
    for (dependent, deps) in edges {
        if !deps.contains(node) || emitted.contains(dependent) {
            continue;
        }
        let Some(degree) = in_degree.get_mut(dependent) else {
            continue;
        };
        *degree = degree.saturating_sub(1);
        if *degree == 0 && queued.insert(dependent.clone()) {
            queue.push_back(dependent.clone());
        }
    }
}

/// Break a cycle by picking the best candidate to start early.
/// Returns false if no candidates remain.
fn break_cycle(in_degree: &mut HashMap<String, usize>, result: &[String]) -> bool {
    let remaining: Vec<_> = in_degree
        .iter()
        .filter(|(n, &deg)| deg > 0 && !result.contains(n))
        .collect();

    if remaining.is_empty() {
        return false;
    }

    let (cycle_node, _) = remaining
        .iter()
        .min_by_key(|(name, &deg)| (unit_type_priority(name), deg))
        .unwrap();

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

    in_degree.insert(cycle_node.to_string(), 0);
    true
}

/// Priority for cycle breaking: lower = start earlier
fn unit_type_priority(name: &str) -> u8 {
    if name.ends_with(".target") {
        0
    } else if name.ends_with(".mount") {
        1
    } else if name.ends_with(".socket") {
        2
    } else if name.ends_with(".path") || name.ends_with(".timer") || name.ends_with(".slice") {
        3
    } else {
        4
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
    use crate::units::{Service, Socket, Target, Unit};

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
    fn alias_and_direct_dependency_iterator_use_canonical_loaded_name() {
        let mut graph = DepGraph::new();
        graph.add_node("canonical.service");
        graph.add_node("consumer.service");
        graph.add_alias("alias.service", "canonical.service");
        graph.add_alias("same.service", "same.service");

        graph.add_edge("consumer.service", "alias.service");

        let deps: Vec<&str> = graph
            .dependencies("consumer.service")
            .map(String::as_str)
            .collect();
        assert_eq!(deps, vec!["canonical.service"]);
        assert!(!graph.aliases.contains_key("same.service"));
    }

    #[test]
    fn add_unit_applies_default_dependencies_to_non_targets() {
        let mut graph = DepGraph::new();
        for name in [
            "sysinit.target",
            "sockets.target",
            "shutdown.target",
            "basic.target",
            "api.socket",
            "api.service",
        ] {
            graph.add_node(name);
        }

        graph.add_unit(&Unit::Socket(Socket::new("api.socket".to_string())));
        graph.add_unit(&Unit::Service(Service::new("api.service".to_string())));
        graph.add_unit(&Unit::Target(Target::new("multi-user.target".to_string())));

        let socket_deps: Vec<&str> = graph
            .dependencies("api.socket")
            .map(String::as_str)
            .collect();
        assert!(socket_deps.contains(&"sysinit.target"));
        assert!(graph
            .dependencies("sockets.target")
            .any(|dep| dep == "api.socket"));
        assert!(graph
            .dependencies("shutdown.target")
            .any(|dep| dep == "api.socket"));

        let service_deps: Vec<&str> = graph
            .dependencies("api.service")
            .map(String::as_str)
            .collect();
        assert!(service_deps.contains(&"basic.target"));
        assert!(graph.dependencies("multi-user.target").next().is_none());
    }

    #[test]
    fn add_unit_with_name_uses_explicit_instance_name_and_wants_dir() {
        let mut graph = DepGraph::new();
        for name in ["template@one.service", "alpha.service", "beta.service"] {
            graph.add_node(name);
        }
        let mut target = Target::new("group.target".to_string());
        target.wants_dir = vec!["alpha.service".to_string(), "beta.service".to_string()];
        target.unit.default_dependencies = false;

        graph.add_unit_with_name("template@one.service", &Unit::Target(target));

        let deps: Vec<&str> = graph
            .dependencies("template@one.service")
            .map(String::as_str)
            .collect();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&"alpha.service"));
        assert!(deps.contains(&"beta.service"));
    }

    #[test]
    fn cycle_error_display_lists_nodes() {
        let error = CycleError {
            nodes: vec!["a.service".to_string(), "b.service".to_string()],
        };

        assert_eq!(
            error.to_string(),
            "Dependency cycle detected involving: a.service, b.service"
        );
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

    #[test]
    fn test_cycle_breaking_prioritizes_sockets() {
        // Simulate dbus-broker.service and dbus.socket in a cycle
        // The cycle breaker should start dbus.socket before dbus-broker.service
        let mut graph = DepGraph::new();

        // Create a cycle: service -> socket -> target -> service
        graph.add_node("dbus.socket");
        graph.add_node("dbus-broker.service");
        graph.add_node("basic.target");

        // dbus-broker.service After=dbus.socket (service depends on socket)
        graph.add_edge("dbus-broker.service", "dbus.socket");
        // dbus.socket After=basic.target - socket waits for target
        graph.add_edge("dbus.socket", "basic.target");
        // basic.target depends on service (creating cycle)
        graph.add_edge("basic.target", "dbus-broker.service");

        // Use start_order_for which breaks cycles via toposort_subset
        let order = graph.start_order_for("dbus-broker.service").unwrap();
        let socket_pos = order.iter().position(|s| s == "dbus.socket").unwrap();
        let service_pos = order
            .iter()
            .position(|s| s == "dbus-broker.service")
            .unwrap();

        // When breaking the cycle, socket should be prioritized over service
        // So dbus.socket should appear before dbus-broker.service
        assert!(
            socket_pos < service_pos,
            "dbus.socket (pos {}) should come before dbus-broker.service (pos {})",
            socket_pos,
            service_pos
        );
    }

    #[test]
    fn test_cycle_breaking_prioritizes_targets_over_sockets() {
        // In a cycle with target, socket, and service, order should be:
        // target -> socket -> service
        let mut graph = DepGraph::new();

        graph.add_node("sysinit.target");
        graph.add_node("dbus.socket");
        graph.add_node("dbus-broker.service");

        // Create cycle: all depend on each other
        graph.add_edge("dbus-broker.service", "dbus.socket");
        graph.add_edge("dbus.socket", "sysinit.target");
        graph.add_edge("sysinit.target", "dbus-broker.service");

        // Use start_order_for which breaks cycles via toposort_subset
        let order = graph.start_order_for("dbus-broker.service").unwrap();
        let target_pos = order.iter().position(|s| s == "sysinit.target").unwrap();
        let socket_pos = order.iter().position(|s| s == "dbus.socket").unwrap();
        let service_pos = order
            .iter()
            .position(|s| s == "dbus-broker.service")
            .unwrap();

        // Priority: target < socket < service
        assert!(
            target_pos < socket_pos,
            "target (pos {}) should come before socket (pos {})",
            target_pos,
            socket_pos
        );
        assert!(
            socket_pos < service_pos,
            "socket (pos {}) should come before service (pos {})",
            socket_pos,
            service_pos
        );
    }
}
