use std::{
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
    kind::{PinType, catalog},
    model::GRAPH_VERSION,
    parse_graph,
};

pub const MAX_GRAPH_DRAFT_BYTES: usize = 4 * 1024 * 1024;
const MAX_JSON_NESTING: usize = 64;

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

    pub fn save(&self, profile_id: &str, graph_json: &str) -> Result<GraphDraftValidationDto, String> {
        let validated = validate_graph_json(graph_json)?;
        let bytes = serde_json::to_vec_pretty(&validated.graph)
            .map_err(|error| format!("Could not serialize local Graph draft: {error}"))?;
        if bytes.len() > MAX_GRAPH_DRAFT_BYTES {
            return Err("Serialized Graph draft exceeds 4 MiB".to_owned());
        }
        let path = self.path_for(profile_id)?;
        atomic_replace(&path, &bytes).map_err(|error| format!("Could not save local Graph draft: {error}"))?;
        Ok(validated)
    }

    pub fn load(&self, profile_id: &str) -> Result<Option<GraphDraftValidationDto>, String> {
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
        if length > MAX_GRAPH_DRAFT_BYTES as u64 {
            return Err("Saved Graph draft exceeds 4 MiB".to_owned());
        }
        let mut bytes = Vec::with_capacity(length as usize);
        file.take((MAX_GRAPH_DRAFT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("Could not read local Graph draft: {error}"))?;
        if bytes.len() > MAX_GRAPH_DRAFT_BYTES {
            return Err("Saved Graph draft exceeds 4 MiB".to_owned());
        }
        let json = String::from_utf8(bytes).map_err(|_| "Saved Graph draft is not UTF-8 JSON".to_owned())?;
        validate_graph_json(&json).map(Some)
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
    graph_json: String,
) -> Result<GraphDraftValidationDto, String> {
    state.graph_drafts.save(&profile_id, &graph_json)
}

#[tauri::command]
pub fn load_graph_draft(
    state: State<'_, AppState>,
    profile_id: String,
) -> Result<Option<GraphDraftValidationDto>, String> {
    state.graph_drafts.load(&profile_id)
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
    fn graph_drafts_save_and_load_per_profile_without_crossing_hosts() {
        let root = temp_dir("profile-isolation");
        let store = GraphDraftStore::open(root.clone()).unwrap();
        let graph = r#"{"version":1,"nodes":[{"id":11,"kind":{"type":"Time"},"pos":[10,20]}],"edges":[]}"#;
        store.save("profile-alpha", graph).expect("save alpha");
        let loaded = store.load("profile-alpha").expect("load alpha").unwrap();
        assert_eq!(loaded.graph["nodes"][0]["id"], 11);
        let replacement = r#"{"version":1,"nodes":[{"id":12,"kind":{"type":"Time"},"pos":[30,40]}],"edges":[]}"#;
        store
            .save("profile-alpha", replacement)
            .expect("replace alpha draft atomically");
        assert_eq!(store.load("profile-alpha").unwrap().unwrap().graph["nodes"][0]["id"], 12);
        assert!(store.load("profile-beta").expect("beta lookup").is_none());
        let escaped = store.path_for("../outside").unwrap();
        assert_eq!(escaped.parent(), Some(root.as_path()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_saved_draft_is_reported_without_replacing_it_with_an_empty_graph() {
        let root = temp_dir("corrupt");
        let store = GraphDraftStore::open(root.clone()).unwrap();
        store.save("profile", r#"{"version":1,"nodes":[],"edges":[]}"#).unwrap();
        let path = store.path_for("profile").unwrap();
        std::fs::write(&path, "not JSON").unwrap();
        assert!(store.load("profile").unwrap_err().contains("invalid graph"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "not JSON");
        std::fs::remove_dir_all(root).unwrap();
    }
}
