use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::State;
use uuid::Uuid;

use crate::app_state::AppState;
use shocking_vrc_core::graph::{
    compile::{Severity, compile},
    kind::{NodeKind, PinType, catalog},
    model::GRAPH_VERSION,
    model::{Graph, NodeId},
    parse_graph, parse_graph_preset,
    preset::GRAPH_PRESET_VERSION,
};

pub const MAX_GRAPH_DRAFT_BYTES: usize = 4 * 1024 * 1024;
const MAX_JSON_NESTING: usize = 64;
const MAX_SAFE_NODE_ID: u64 = (1_u64 << 53) - 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphPinDto {
    pub key: String,
    pub label: String,
    pub value_type: String,
    pub default: f32,
    pub min: f32,
    pub max: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphNodeTemplateDto {
    pub id: String,
    pub label: String,
    pub category: String,
    pub keywords: String,
    pub kind: Value,
    pub inputs: Vec<GraphPinDto>,
    pub outputs: Vec<GraphPinDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GraphNodeCatalogDto {
    pub entries: Vec<GraphNodeTemplateDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphDraftIssueDto {
    pub severity: String,
    pub node_id: Option<u64>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphDraftValidationDto {
    pub graph: Value,
    pub valid: bool,
    pub issues: Vec<GraphDraftIssueDto>,
    pub missing_types: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredGraphDraftDto {
    pub base_revision: Option<u64>,
    #[serde(flatten)]
    pub validation: GraphDraftValidationDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum GraphDraftTransformDto {
    Group { node_ids: Vec<u64>, name: String },
    Ungroup { node_id: u64 },
    Duplicate { node_ids: Vec<u64>, offset: [f32; 2] },
    ReplacePreset { preset_json: String },
    InsertPreset { preset_json: String, position: [f32; 2] },
}

#[derive(Debug, Serialize, Deserialize)]
struct GraphDraftFile {
    schema_version: u32,
    base_revision: Option<u64>,
    graph: Value,
}

pub struct GraphDraftStore {
    root: PathBuf,
}

impl GraphDraftStore {
    pub fn open(root: PathBuf) -> Result<Self, String> {
        Ok(Self { root })
    }

    fn path_for(&self, profile_id: &str) -> Result<PathBuf, String> {
        if profile_id.trim().is_empty() || profile_id.len() > 256 {
            return Err("Invalid Host profile for Graph draft storage".to_owned());
        }
        let digest = Sha256::digest(profile_id.as_bytes());
        Ok(self.root.join(format!("{:x}.graph.json", digest)))
    }

    pub fn save(
        &self,
        profile_id: &str,
        base_revision: Option<u64>,
        graph_json: &str,
    ) -> Result<StoredGraphDraftDto, String> {
        let validated = validate_graph_json(graph_json)?;
        let bytes = serde_json::to_vec(&GraphDraftFile {
            schema_version: 1,
            base_revision,
            graph: validated.graph.clone(),
        })
        .map_err(|error| format!("Could not serialize local Graph draft: {error}"))?;
        if bytes.len() > MAX_GRAPH_DRAFT_BYTES.saturating_add(256) {
            return Err("Serialized Graph draft file exceeds its bounded size".to_owned());
        }
        let path = self.path_for(profile_id)?;
        atomic_replace(&path, &bytes).map_err(|error| format!("Could not save local Graph draft: {error}"))?;
        Ok(StoredGraphDraftDto {
            base_revision,
            validation: validated,
        })
    }

    pub fn load(&self, profile_id: &str) -> Result<Option<StoredGraphDraftDto>, String> {
        let path = self.path_for(profile_id)?;
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("Could not read local Graph draft: {error}")),
        };
        let length = file
            .metadata()
            .map_err(|error| format!("Could not inspect local Graph draft: {error}"))?
            .len();
        if length > MAX_GRAPH_DRAFT_BYTES.saturating_add(256) as u64 {
            return Err("Saved Graph draft file exceeds its bounded size".to_owned());
        }
        let mut bytes = Vec::with_capacity(length as usize);
        file.take((MAX_GRAPH_DRAFT_BYTES + 257) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Could not read local Graph draft: {error}"))?;
        if bytes.len() > MAX_GRAPH_DRAFT_BYTES.saturating_add(256) {
            return Err("Saved Graph draft file exceeds its bounded size".to_owned());
        }
        let json = String::from_utf8(bytes).map_err(|_| "Saved Graph draft is not UTF-8 JSON".to_owned())?;
        let value: Value = serde_json::from_str(&json).map_err(|_| "Saved Graph draft is not valid JSON".to_owned())?;
        let (base_revision, graph_json) = if value.get("schema_version").is_some() {
            let file: GraphDraftFile =
                serde_json::from_value(value).map_err(|_| "Saved Graph draft metadata is invalid".to_owned())?;
            if file.schema_version != 1 {
                return Err("Saved Graph draft uses an unsupported file version".to_owned());
            }
            (
                file.base_revision,
                serde_json::to_string(&file.graph)
                    .map_err(|_| "Saved Graph draft could not be normalized".to_owned())?,
            )
        } else {
            // Older local drafts have no revision provenance, so they must be shown as stale.
            (None, json)
        };
        let validation = validate_graph_json(&graph_json)?;
        Ok(Some(StoredGraphDraftDto {
            base_revision,
            validation,
        }))
    }
}

#[tauri::command]
pub fn get_graph_node_catalog() -> GraphNodeCatalogDto {
    let entries = catalog()
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            let kind = (entry.make)();
            let inputs = kind.inputs().iter().map(pin_dto).collect();
            let outputs = kind.outputs().iter().map(pin_dto).collect();
            let kind_json = serde_json::to_value(&kind).expect("Host Graph node catalog is serializable");
            GraphNodeTemplateDto {
                id: format!("{}-{index}", kind.type_name()),
                label: entry.label.to_owned(),
                category: entry.category.label().to_owned(),
                keywords: entry.keywords.to_owned(),
                kind: kind_json,
                inputs,
                outputs,
            }
        })
        .collect();
    GraphNodeCatalogDto { entries }
}

fn pin_dto(pin: &shocking_vrc_core::graph::PinSpec) -> GraphPinDto {
    GraphPinDto {
        key: pin.key.to_string(),
        label: pin.label.to_string(),
        value_type: match pin.ty {
            PinType::Float => "Float",
            PinType::Bool => "Bool",
        }
        .to_owned(),
        default: pin.default,
        min: pin.range.0,
        max: pin.range.1,
    }
}

pub fn validate_graph_json(graph_json: &str) -> Result<GraphDraftValidationDto, String> {
    if graph_json.len() > MAX_GRAPH_DRAFT_BYTES {
        return Err("Graph draft exceeds 4 MiB".to_owned());
    }
    check_json_nesting(graph_json.as_bytes())?;
    let graph = parse_graph(graph_json)?;
    if graph.version != GRAPH_VERSION {
        return Err(format!("Unsupported Graph version {}; expected {GRAPH_VERSION}", graph.version));
    }
    validate_graph_node_id_precision(&graph)?;
    let missing_types = graph.missing_types();
    let plan = compile(&graph);
    let mut issues: Vec<GraphDraftIssueDto> = plan
        .issues
        .into_iter()
        .map(|issue| GraphDraftIssueDto {
            severity: match issue.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Info => "info",
            }
            .to_owned(),
            node_id: issue.node.map(|id| id.0),
            message: issue.message,
        })
        .collect();
    for missing in &missing_types {
        issues.push(GraphDraftIssueDto {
            severity: "error".to_owned(),
            node_id: None,
            message: format!("Unknown node type '{missing}' is preserved as a read-only placeholder"),
        });
    }
    let valid = missing_types.is_empty() && !issues.iter().any(|issue| issue.severity == "error");
    let graph = serde_json::to_value(graph).map_err(|error| format!("Could not normalize Graph draft: {error}"))?;
    Ok(GraphDraftValidationDto {
        graph,
        valid,
        issues,
        missing_types,
    })
}

pub fn validate_graph_node_id_precision_value(graph: &Value) -> Result<(), String> {
    let parsed: Graph =
        serde_json::from_value(graph.clone()).map_err(|_| "Host returned a malformed Graph document".to_owned())?;
    validate_graph_node_id_precision(&parsed)
}

fn validate_graph_node_id_precision(graph: &Graph) -> Result<(), String> {
    for node in &graph.nodes {
        if node.id.0 == 0 || node.id.0 > MAX_SAFE_NODE_ID {
            return Err("Graph node IDs must be positive JavaScript-safe integers".to_owned());
        }
        if let NodeKind::Group(group) = &node.kind {
            validate_graph_node_id_precision(&group.graph)?;
        }
    }
    for edge in &graph.edges {
        if edge.from.node.0 > MAX_SAFE_NODE_ID || edge.to.node.0 > MAX_SAFE_NODE_ID {
            return Err("Graph wire references a node ID JavaScript cannot represent exactly".to_owned());
        }
    }
    Ok(())
}

fn check_json_nesting(bytes: &[u8]) -> Result<(), String> {
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    for &byte in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_JSON_NESTING {
                    return Err(format!("Graph draft JSON is nested deeper than {MAX_JSON_NESTING} levels"));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Graph draft path has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = parent.join(format!(".{file_name}.{}.part", Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW};

    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[tauri::command]
pub fn validate_graph_draft(graph_json: String) -> Result<GraphDraftValidationDto, String> {
    validate_graph_json(&graph_json)
}

#[tauri::command]
pub fn save_graph_draft(
    state: State<'_, AppState>,
    profile_id: String,
    base_revision: Option<u64>,
    graph_json: String,
) -> Result<StoredGraphDraftDto, String> {
    state.graph_drafts.save(&profile_id, base_revision, &graph_json)
}

#[tauri::command]
pub fn load_graph_draft(state: State<'_, AppState>, profile_id: String) -> Result<Option<StoredGraphDraftDto>, String> {
    state.graph_drafts.load(&profile_id)
}

#[tauri::command]
pub fn transform_graph_draft(
    graph_json: String,
    operation: GraphDraftTransformDto,
) -> Result<GraphDraftValidationDto, String> {
    let normalized = validate_graph_json(&graph_json)?;
    let normalized_json =
        serde_json::to_string(&normalized.graph).map_err(|_| "Graph draft could not be normalized".to_owned())?;
    let mut graph = parse_graph(&normalized_json)?;
    match operation {
        GraphDraftTransformDto::Group { node_ids, name } => {
            validate_transform_ids(&graph, &node_ids)?;
            let name = name.trim();
            if name.is_empty() || name.len() > 128 {
                return Err("Graph group name must be between 1 and 128 characters".to_owned());
            }
            let ids = node_ids.into_iter().map(NodeId).collect::<HashSet<_>>();
            graph
                .group(&ids, name)
                .ok_or_else(|| "Host Graph could not group this selection".to_owned())?;
        }
        GraphDraftTransformDto::Ungroup { node_id } => {
            let id = NodeId(node_id);
            if !graph
                .node(id)
                .is_some_and(|node| matches!(&node.kind, NodeKind::Group(_)))
            {
                return Err("Selected Graph node is not a group".to_owned());
            }
            graph.ungroup(id);
        }
        GraphDraftTransformDto::Duplicate { node_ids, offset } => {
            validate_transform_ids(&graph, &node_ids)?;
            validate_transform_offset(offset)?;
            let ids = node_ids.into_iter().map(NodeId).collect::<HashSet<_>>();
            let selected = graph.extract(&ids);
            graph.insert(&selected, offset);
        }
        GraphDraftTransformDto::ReplacePreset { preset_json } => {
            let preset = parse_graph_preset(&preset_json)?;
            if preset.version != GRAPH_PRESET_VERSION {
                return Err(format!("Unsupported Graph preset version {}", preset.version));
            }
            graph = preset.instantiate(&[]);
        }
        GraphDraftTransformDto::InsertPreset { preset_json, position } => {
            validate_transform_offset(position)?;
            let preset = parse_graph_preset(&preset_json)?;
            if preset.version != GRAPH_PRESET_VERSION {
                return Err(format!("Unsupported Graph preset version {}", preset.version));
            }
            preset.insert_into(&mut graph, position, &[]);
        }
    }
    let transformed =
        serde_json::to_string(&graph).map_err(|_| "Transformed Graph draft could not be serialized".to_owned())?;
    validate_graph_json(&transformed)
}

fn validate_transform_ids(graph: &Graph, ids: &[u64]) -> Result<(), String> {
    if ids.is_empty() || ids.len() > 4096 {
        return Err("Graph transform selection is empty or too large".to_owned());
    }
    let unique: HashSet<u64> = ids.iter().copied().collect();
    if unique.len() != ids.len() || ids.iter().any(|id| graph.node(NodeId(*id)).is_none()) {
        return Err("Graph transform selection contains duplicate or unknown node IDs".to_owned());
    }
    Ok(())
}

fn validate_transform_offset(offset: [f32; 2]) -> Result<(), String> {
    if offset
        .iter()
        .any(|value| !value.is_finite() || value.abs() > 1_000_000.0)
    {
        return Err("Graph transform position is outside the supported range".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shocking_vrc_core::graph::kind::catalog;

    fn temp_dir(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("shockingvrc-graph-draft-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn node_catalog_matches_host_kinds_and_exports_typed_pins() {
        let actual = get_graph_node_catalog();
        let host = catalog();
        assert_eq!(actual.entries.len(), host.len());
        let constant = actual.entries.iter().find(|entry| entry.label == "Constant").unwrap();
        assert_eq!(constant.kind["type"], "Constant");
        assert_eq!(constant.category, "Inputs");
        let pin = constant.outputs.iter().find(|pin| pin.key == "out").unwrap();
        assert_eq!(pin.value_type, "Float");
        assert_eq!(pin.min, 0.0);
        assert_eq!(pin.max, 1.0);
        let logic = actual.entries.iter().find(|entry| entry.label == "And").unwrap();
        assert_eq!(logic.outputs[0].value_type, "Bool");
    }

    #[test]
    fn graph_validation_uses_the_core_parser_and_compile_issues() {
        let graph = r#"{"version":1,"nodes":[{"id":1,"kind":{"type":"Constant","value":0.5},"pos":[0,0]}],"edges":[]}"#;
        let result = validate_graph_json(graph).expect("valid Graph parses");
        assert_eq!(result.graph["nodes"][0]["kind"]["type"], "Constant");
        assert!(result.valid);

        let unknown =
            r#"{"version":1,"nodes":[{"id":2,"kind":{"type":"FutureNode","secret":false},"pos":[0,0]}],"edges":[]}"#;
        let result = validate_graph_json(unknown).expect("unknown nodes stay round-trippable");
        assert!(!result.valid);
        assert_eq!(result.missing_types, vec!["FutureNode"]);
    }

    #[test]
    fn graph_validation_rejects_bad_json_oversize_and_excessive_nesting() {
        assert!(validate_graph_json("not JSON").is_err());
        assert!(
            validate_graph_json(r#"{"version":2,"nodes":[],"edges":[]}"#)
                .unwrap_err()
                .contains("Unsupported Graph version")
        );
        assert!(
            validate_graph_json(&" ".repeat(4 * 1024 * 1024 + 1))
                .unwrap_err()
                .contains("4 MiB")
        );
        let deeply_nested = format!("{}{}", "[".repeat(80), "]".repeat(80));
        assert!(validate_graph_json(&deeply_nested).unwrap_err().contains("nested"));
    }

    #[test]
    fn graph_validation_rejects_node_ids_that_javascript_cannot_represent_exactly() {
        let unsafe_id =
            r#"{"version":1,"nodes":[{"id":9007199254740992,"kind":{"type":"Constant","value":0.5}}],"edges":[]}"#;
        assert!(validate_graph_json(unsafe_id).is_err());
    }

    #[test]
    fn graph_drafts_save_and_load_per_profile_without_crossing_hosts() {
        let root = temp_dir("profile-isolation");
        let store = GraphDraftStore::open(root.clone()).unwrap();
        let graph = r#"{"version":1,"nodes":[{"id":11,"kind":{"type":"Time"},"pos":[10,20]}],"edges":[]}"#;
        store.save("profile-alpha", Some(44), graph).expect("save alpha");
        let loaded = store.load("profile-alpha").expect("load alpha").unwrap();
        assert_eq!(loaded.base_revision, Some(44));
        assert_eq!(loaded.validation.graph["nodes"][0]["id"], 11);
        let replacement = r#"{"version":1,"nodes":[{"id":12,"kind":{"type":"Time"},"pos":[30,40]}],"edges":[]}"#;
        store
            .save("profile-alpha", Some(44), replacement)
            .expect("replace alpha draft atomically");
        assert_eq!(
            store.load("profile-alpha").unwrap().unwrap().validation.graph["nodes"][0]["id"],
            12
        );
        assert!(store.load("profile-beta").expect("beta lookup").is_none());
        let escaped = store.path_for("../outside").unwrap();
        assert_eq!(escaped.parent(), Some(root.as_path()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_saved_draft_is_reported_without_replacing_it_with_an_empty_graph() {
        let root = temp_dir("corrupt");
        let store = GraphDraftStore::open(root.clone()).unwrap();
        store
            .save("profile", Some(3), r#"{"version":1,"nodes":[],"edges":[]}"#)
            .unwrap();
        let path = store.path_for("profile").unwrap();
        std::fs::write(&path, "not JSON").unwrap();
        assert!(store.load("profile").unwrap_err().contains("valid JSON"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "not JSON");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn old_graph_drafts_load_without_revision_and_must_be_treated_as_stale() {
        let root = temp_dir("legacy");
        let store = GraphDraftStore::open(root.clone()).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(store.path_for("profile").unwrap(), r#"{"version":1,"nodes":[],"edges":[]}"#).unwrap();
        let loaded = store.load("profile").unwrap().unwrap();
        assert_eq!(loaded.base_revision, None);
        assert!(loaded.validation.valid);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn transforms_reuse_core_group_duplicate_and_preset_semantics() {
        let source = r#"{"version":1,"nodes":[
            {"id":1,"kind":{"type":"Constant","value":0.25},"pos":[0,0]},
            {"id":2,"kind":{"type":"Time"},"pos":[300,0]}
        ],"edges":[]}"#;
        let duplicated = transform_graph_draft(
            source.to_owned(),
            GraphDraftTransformDto::Duplicate {
                node_ids: vec![1],
                offset: [40.0, 40.0],
            },
        )
        .unwrap();
        assert_eq!(duplicated.graph["nodes"].as_array().unwrap().len(), 3);

        let grouped = transform_graph_draft(
            source.to_owned(),
            GraphDraftTransformDto::Group {
                node_ids: vec![1, 2],
                name: "Pair".to_owned(),
            },
        )
        .unwrap();
        assert_eq!(grouped.graph["nodes"].as_array().unwrap().len(), 1);
        assert_eq!(grouped.graph["nodes"][0]["kind"]["type"], "Group");

        let ungrouped = transform_graph_draft(
            serde_json::to_string(&grouped.graph).unwrap(),
            GraphDraftTransformDto::Ungroup {
                node_id: grouped.graph["nodes"][0]["id"].as_u64().unwrap(),
            },
        )
        .unwrap();
        assert_eq!(ungrouped.graph["nodes"].as_array().unwrap().len(), 2);

        let preset = r#"{"version":1,"name":"Single","graph":{"version":1,"nodes":[{"id":1,"kind":{"type":"Constant","value":0.5},"pos":[0,0]}],"edges":[]}}"#;
        let replaced = transform_graph_draft(
            source.to_owned(),
            GraphDraftTransformDto::ReplacePreset {
                preset_json: preset.to_owned(),
            },
        )
        .unwrap();
        assert_eq!(replaced.graph["nodes"].as_array().unwrap().len(), 1);
        let inserted = transform_graph_draft(
            source.to_owned(),
            GraphDraftTransformDto::InsertPreset {
                preset_json: preset.to_owned(),
                position: [600.0, 250.0],
            },
        )
        .unwrap();
        assert_eq!(inserted.graph["nodes"].as_array().unwrap().len(), 3);
    }
}
