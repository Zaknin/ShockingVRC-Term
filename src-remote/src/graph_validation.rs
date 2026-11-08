use std::collections::{HashMap, HashSet};

use serde_json::Value;
use shocking_vrc_core::graph::{Graph, Node, NodeId, compile, kind::NodeKind, model::GRAPH_VERSION};

pub const MAX_GRAPH_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_JSON_DEPTH: usize = 64;
const MAX_GROUP_DEPTH: usize = 8;
const MAX_GRAPH_NODES: usize = 4096;
const MAX_GRAPH_EDGES: usize = 16384;
const MAX_SAFE_NODE_ID: u64 = (1_u64 << 53) - 1;

#[derive(Default)]
struct GraphShape {
    nodes: usize,
    edges: usize,
}

pub fn validate_graph_json(bytes: &[u8]) -> Result<Graph, String> {
    if bytes.is_empty() || bytes.len() > MAX_GRAPH_DOCUMENT_BYTES {
        return Err("Graph document must contain 1 to 4 MiB".into());
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|error| format!("Invalid Graph JSON: {error}"))?;
    validate_json_depth(&value, 0)?;
    validate_explicit_versions(&value)?;

    let graph: Graph = serde_json::from_value(value).map_err(|error| format!("Invalid Graph document: {error}"))?;
    let mut shape = GraphShape::default();
    validate_graph_shape(&graph, 0, &mut shape)?;

    let mut sanitized = graph.clone();
    sanitized.sanitize();
    if sanitized != graph {
        return Err("Graph contains values the Host would change during sanitization".into());
    }

    let plan = compile::compile(&graph);
    if !plan.valid
        || plan
            .issues
            .iter()
            .any(|issue| issue.severity == shocking_vrc_core::graph::Severity::Error)
    {
        return Err("Graph contains a cycle or compile error".into());
    }
    Ok(graph)
}

fn validate_json_depth(value: &Value, depth: usize) -> Result<(), String> {
    if depth > MAX_JSON_DEPTH {
        return Err("Graph JSON exceeds the maximum nesting depth".into());
    }
    match value {
        Value::Array(values) => values
            .iter()
            .try_for_each(|value| validate_json_depth(value, depth + 1)),
        Value::Object(values) => values
            .values()
            .try_for_each(|value| validate_json_depth(value, depth + 1)),
        _ => Ok(()),
    }
}

fn validate_explicit_versions(value: &Value) -> Result<(), String> {
    let Some(object) = value.as_object() else {
        return Err("Graph document must be an object".into());
    };
    let Some(version) = object.get("version").and_then(Value::as_u64) else {
        return Err("Graph version is required".into());
    };
    if version != u64::from(GRAPH_VERSION) {
        return Err(format!("Unsupported Graph version {version}"));
    }
    let nodes = object
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "Graph nodes must be an array".to_owned())?;
    for node in nodes {
        if node.pointer("/kind/type").and_then(Value::as_str) == Some("Group") {
            let nested = node
                .pointer("/kind/graph")
                .ok_or_else(|| "Group node is missing its nested Graph".to_owned())?;
            validate_explicit_versions(nested)?;
        }
    }
    Ok(())
}

fn validate_graph_shape(graph: &Graph, group_depth: usize, shape: &mut GraphShape) -> Result<(), String> {
    if graph.version != GRAPH_VERSION {
        return Err(format!("Unsupported Graph version {}", graph.version));
    }
    shape.nodes = shape.nodes.saturating_add(graph.nodes.len());
    shape.edges = shape.edges.saturating_add(graph.edges.len());
    if shape.nodes > MAX_GRAPH_NODES || shape.edges > MAX_GRAPH_EDGES {
        return Err("Graph exceeds the Host node or wire limit".into());
    }

    let mut nodes = HashMap::<NodeId, &Node>::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        if node.id.0 == 0 || node.id.0 > MAX_SAFE_NODE_ID || nodes.insert(node.id, node).is_some() {
            return Err("Graph node IDs must be unique, positive, and exactly representable by Terminal".into());
        }
        if node.kind.is_missing() {
            return Err(format!("Graph contains unsupported node type '{}'", node.kind.type_name()));
        }
        if let NodeKind::Group(group) = &node.kind {
            let next_depth = group_depth + 1;
            if next_depth > MAX_GROUP_DEPTH {
                return Err("Graph groups are nested too deeply".into());
            }
            validate_graph_shape(&group.graph, next_depth, shape)?;
        }

        let inputs = node.inputs();
        let outputs = node.outputs();
        validate_pin_set(&inputs)?;
        validate_pin_set(&outputs)?;
        for key in node.defaults.keys() {
            if !inputs.iter().any(|pin| pin.key == key.as_str()) {
                return Err(format!("Node {} has a default for an unknown input pin", node.id.0));
            }
        }
    }

    for edge in &graph.edges {
        let source = nodes
            .get(&edge.from.node)
            .ok_or_else(|| format!("Graph wire references missing source node {}", edge.from.node.0))?;
        let target = nodes
            .get(&edge.to.node)
            .ok_or_else(|| format!("Graph wire references missing target node {}", edge.to.node.0))?;
        let source_pin = source
            .outputs()
            .into_iter()
            .find(|pin| pin.key == edge.from.pin)
            .ok_or_else(|| format!("Graph wire references an unknown source pin on node {}", source.id.0))?;
        let target_pin = target
            .inputs()
            .into_iter()
            .find(|pin| pin.key == edge.to.pin)
            .ok_or_else(|| format!("Graph wire references an unknown target pin on node {}", target.id.0))?;
        if source_pin.ty != target_pin.ty {
            return Err("Graph wire connects pins with incompatible value types".into());
        }
    }
    Ok(())
}

fn validate_pin_set(pins: &[shocking_vrc_core::graph::PinSpec]) -> Result<(), String> {
    let mut keys = HashSet::with_capacity(pins.len());
    for pin in pins {
        if pin.key.is_empty() || !keys.insert(pin.key.as_ref()) {
            return Err("Graph node contains an empty or duplicate pin key".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_graph_json;
    use serde_json::json;

    #[test]
    fn accepts_a_supported_empty_graph() {
        let graph = validate_graph_json(br#"{"version":1,"nodes":[],"edges":[]}"#).unwrap();
        assert_eq!(graph.version, 1);
        assert!(graph.nodes.is_empty());
    }

    #[test]
    fn rejects_missing_or_unsupported_graph_versions() {
        for value in [
            json!({"nodes": [], "edges": []}),
            json!({"version": 2, "nodes": [], "edges": []}),
        ] {
            assert!(validate_graph_json(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }

    #[test]
    fn rejects_unknown_node_types_instead_of_preserving_placeholders() {
        let value = json!({
            "version": 1,
            "nodes": [{"id": 1, "kind": {"type": "FutureNode", "value": 1.0}}],
            "edges": []
        });
        assert!(validate_graph_json(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn rejects_dangling_and_wrong_direction_pin_references() {
        let dangling = json!({
            "version": 1,
            "nodes": [{"id": 1, "kind": {"type": "Constant", "value": 0.5}}],
            "edges": [{"from": {"node": 1, "pin": "out"}, "to": {"node": 99, "pin": "in"}}]
        });
        assert!(validate_graph_json(&serde_json::to_vec(&dangling).unwrap()).is_err());

        let wrong_direction = json!({
            "version": 1,
            "nodes": [
                {"id": 1, "kind": {"type": "Constant", "value": 0.5}},
                {"id": 2, "kind": {"type": "Reroute"}}
            ],
            "edges": [{"from": {"node": 2, "pin": "out"}, "to": {"node": 1, "pin": "out"}}]
        });
        assert!(validate_graph_json(&serde_json::to_vec(&wrong_direction).unwrap()).is_err());
    }

    #[test]
    fn rejects_type_mismatches_and_duplicate_node_ids() {
        let type_mismatch = json!({
            "version": 1,
            "nodes": [
                {"id": 1, "kind": {"type": "Toggle"}},
                {"id": 2, "kind": {"type": "Reroute"}}
            ],
            "edges": [{"from": {"node": 1, "pin": "out"}, "to": {"node": 2, "pin": "in"}}]
        });
        assert!(validate_graph_json(&serde_json::to_vec(&type_mismatch).unwrap()).is_err());

        let duplicate = json!({
            "version": 1,
            "nodes": [
                {"id": 1, "kind": {"type": "Constant", "value": 0.5}},
                {"id": 1, "kind": {"type": "Constant", "value": 0.25}}
            ],
            "edges": []
        });
        assert!(validate_graph_json(&serde_json::to_vec(&duplicate).unwrap()).is_err());
    }

    #[test]
    fn rejects_node_ids_that_terminal_javascript_cannot_represent_exactly() {
        let value = json!({
            "version": 1,
            "nodes": [{"id": 9_007_199_254_740_992_u64, "kind": {"type": "Constant", "value": 0.5}}],
            "edges": []
        });
        assert!(validate_graph_json(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn requires_a_supported_version_in_every_nested_group_graph() {
        let value = json!({
            "version": 1,
            "nodes": [{
                "id": 1,
                "kind": {"type": "Group", "name": "G", "graph": {"nodes": [], "edges": []}}
            }],
            "edges": []
        });
        assert!(validate_graph_json(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn bounds_total_node_count_across_nested_graphs() {
        let nodes = (1..=4097_u64)
            .map(|id| json!({"id": id, "kind": {"type": "Frame"}}))
            .collect::<Vec<_>>();
        let value = json!({"version": 1, "nodes": nodes, "edges": []});
        assert!(validate_graph_json(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn rejects_cycles_without_a_delay_node() {
        let value = json!({
            "version": 1,
            "nodes": [
                {"id": 1, "kind": {"type": "Reroute"}},
                {"id": 2, "kind": {"type": "Reroute"}}
            ],
            "edges": [
                {"from": {"node": 1, "pin": "out"}, "to": {"node": 2, "pin": "in"}},
                {"from": {"node": 2, "pin": "out"}, "to": {"node": 1, "pin": "in"}}
            ]
        });
        assert!(validate_graph_json(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn rejects_values_the_host_would_silently_sanitize() {
        let value = json!({
            "version": 1,
            "nodes": [{"id": 1, "kind": {"type": "Slider", "min": 0.0, "max": 1.0, "step": 0.1, "value": 4.0}}],
            "edges": []
        });
        assert!(validate_graph_json(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn bounds_document_bytes_and_json_nesting() {
        let oversized = vec![b' '; super::MAX_GRAPH_DOCUMENT_BYTES + 1];
        assert!(validate_graph_json(&oversized).is_err());

        let nested = format!(
            "{{\"version\":1,\"nodes\":[],\"edges\":[],\"extra\":{}}}",
            "[".repeat(70) + "0" + &"]".repeat(70)
        );
        assert!(validate_graph_json(nested.as_bytes()).is_err());
    }
}
