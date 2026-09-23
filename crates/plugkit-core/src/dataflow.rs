use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pkfs;

pub const DATAFLOW_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepNode {
    pub id: String,
    pub plugin: String,
    pub verb: String,
    #[serde(default)]
    pub input: InputMapping,
    #[serde(default)]
    pub when: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InputMapping {
    #[serde(default)]
    pub fields: std::collections::BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FuseNode {
    pub id: String,
    pub strategy: String,
    pub sources: Vec<String>,
    #[serde(default)]
    pub params: std::collections::BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Condition {
    pub name: String,
    pub field: String,
    pub equals: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pipeline {
    pub entry_point: String,
    #[serde(default)]
    pub steps: Vec<StepNode>,
    #[serde(default)]
    pub fuse: Vec<FuseNode>,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    pub output: String,
}

impl Pipeline {
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.entry_point.trim().is_empty() {
            problems.push("entry_point must be non-empty".to_string());
        }
        let mut seen_ids = std::collections::HashSet::new();
        for s in &self.steps {
            if s.id.trim().is_empty() {
                problems.push("a step has an empty id".to_string());
                continue;
            }
            if !seen_ids.insert(s.id.clone()) {
                problems.push(format!("duplicate step id `{}`", s.id));
            }
            if s.plugin.trim().is_empty() || s.verb.trim().is_empty() {
                problems.push(format!("step `{}` must name a non-empty plugin and verb", s.id));
            }
            if let Some(cond) = &s.when {
                if !self.conditions.iter().any(|c| &c.name == cond) {
                    problems.push(format!("step `{}` references unknown condition `{cond}`", s.id));
                }
            }
        }
        for f in &self.fuse {
            if f.id.trim().is_empty() {
                problems.push("a fuse node has an empty id".to_string());
                continue;
            }
            if !seen_ids.insert(f.id.clone()) {
                problems.push(format!("duplicate step/fuse id `{}`", f.id));
            }
            if f.sources.is_empty() {
                problems.push(format!("fuse node `{}` names no sources", f.id));
            }
            for src in &f.sources {
                if !self.steps.iter().any(|s| &s.id == src) && !self.fuse.iter().any(|other| &other.id == src && other.id != f.id) {
                    problems.push(format!("fuse node `{}` names unknown source `{src}`", f.id));
                }
            }
            if !known_fuse_strategy(&f.strategy) {
                problems.push(format!("fuse node `{}` names unknown strategy `{}`", f.id, f.strategy));
            }
        }
        if self.output.trim().is_empty() {
            problems.push("output must name a step or fuse-node id".to_string());
        } else if !seen_ids.contains(&self.output) {
            problems.push(format!("output `{}` does not name any step or fuse node", self.output));
        }
        problems
    }
}

fn known_fuse_strategy(name: &str) -> bool {
    matches!(name, "rrf_fuse" | "present_both")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DataflowDocument {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub pipelines: std::collections::BTreeMap<String, Pipeline>,
}

impl DataflowDocument {
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (name, p) in &self.pipelines {
            if &p.entry_point != name {
                problems.push(format!(
                    "pipelines key `{name}` does not match its own entry_point `{}`",
                    p.entry_point
                ));
            }
            for problem in p.validate() {
                problems.push(format!("{name}: {problem}"));
            }
        }
        problems
    }
}

const DATAFLOW_OVERRIDE_PATH: &str = ".gm/instructions/dataflow/graph.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataflowTier {
    LocalOverride,
    SourceRepo,
    CompiledDefault,
}

impl DataflowTier {
    pub fn as_str(self) -> &'static str {
        match self {
            DataflowTier::LocalOverride => "local_override",
            DataflowTier::SourceRepo => "source_repo",
            DataflowTier::CompiledDefault => "compiled_default",
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn source_repo_dataflow_path() -> Option<String> {
    let resolved = crate::config::resolve();
    let rel = resolved
        .config
        .value
        .get("dataflow")
        .and_then(|f| f.get("graph"))
        .and_then(|g| g.as_str())?;
    let rel = rel.trim().trim_matches('/');
    if rel.is_empty() {
        return None;
    }
    if crate::config_path::validate_source_path(rel).is_err() {
        return None;
    }
    let base = resolved.cache_dir?;
    Some(format!("{}/{rel}", base.trim_end_matches(['/', '\\'])))
}

#[cfg(not(target_arch = "wasm32"))]
fn source_repo_dataflow_path() -> Option<String> {
    None
}

const COMPILED_PATH: &str = "<compiled default>";

fn load_tier(raw: &str) -> Option<DataflowDocument> {
    let doc: DataflowDocument = serde_json::from_str(raw).ok()?;
    if !doc.validate().is_empty() {
        return None;
    }
    Some(doc)
}

pub fn document_detailed() -> (DataflowDocument, DataflowTier, String) {
    if let Some(raw) = pkfs::read_to_string(DATAFLOW_OVERRIDE_PATH) {
        if let Some(doc) = load_tier(&raw) {
            return (doc, DataflowTier::LocalOverride, DATAFLOW_OVERRIDE_PATH.to_string());
        }
    }
    if let Some(path) = source_repo_dataflow_path() {
        if let Some(raw) = pkfs::read_to_string(&path) {
            if let Some(doc) = load_tier(&raw) {
                return (doc, DataflowTier::SourceRepo, path);
            }
        }
    }
    (default_document(), DataflowTier::CompiledDefault, COMPILED_PATH.to_string())
}

pub fn document() -> DataflowDocument {
    document_detailed().0
}

pub fn pipeline_for(entry_point: &str) -> Option<Pipeline> {
    document().pipelines.remove(entry_point)
}

fn default_document() -> DataflowDocument {
    let mut pipelines = std::collections::BTreeMap::new();

    pipelines.insert(
        "codesearch".to_string(),
        Pipeline {
            entry_point: "codesearch".to_string(),
            steps: vec![
                StepNode {
                    id: "embed".to_string(),
                    plugin: "gm".to_string(),
                    verb: "embed_query".to_string(),
                    input: InputMapping { fields: btreemap([("query", "request.query")]) },
                    when: None,
                },
                StepNode {
                    id: "vector_search".to_string(),
                    plugin: "gm".to_string(),
                    verb: "vector_search".to_string(),
                    input: InputMapping {
                        fields: btreemap([("embedding", "steps.embed.embedding"), ("namespace", "request.code_namespace"), ("k", "request.k")]),
                    },
                    when: None,
                },
                StepNode {
                    id: "bm25".to_string(),
                    plugin: "gm".to_string(),
                    verb: "bm25_rank".to_string(),
                    input: InputMapping { fields: btreemap([("query", "request.query"), ("k", "request.cand_k")]) },
                    when: None,
                },
                StepNode {
                    id: "commits".to_string(),
                    plugin: "gm".to_string(),
                    verb: "git_commit_rank".to_string(),
                    input: InputMapping { fields: btreemap([("query", "request.query"), ("limit", "literal:10")]) },
                    when: None,
                },
            ],
            fuse: vec![FuseNode {
                id: "presented".to_string(),
                strategy: "present_both".to_string(),
                sources: vec!["vector_search".to_string(), "bm25".to_string(), "commits".to_string()],
                params: std::collections::BTreeMap::new(),
            }],
            conditions: vec![],
            output: "presented".to_string(),
        },
    );

    pipelines.insert(
        "code_index".to_string(),
        Pipeline {
            entry_point: "code_index".to_string(),
            steps: vec![
                StepNode {
                    id: "chunk".to_string(),
                    plugin: "gm".to_string(),
                    verb: "extract_chunks".to_string(),
                    input: InputMapping {
                        fields: btreemap([("path", "request.path"), ("source", "request.source"), ("lang", "request.lang")]),
                    },
                    when: None,
                },
                StepNode {
                    id: "embed_batch".to_string(),
                    plugin: "bert".to_string(),
                    verb: "embed_batch".to_string(),
                    input: InputMapping { fields: btreemap([("texts", "steps.chunk.bodies")]) },
                    when: None,
                },
                StepNode {
                    id: "cache_write".to_string(),
                    plugin: "libsql".to_string(),
                    verb: "upsert_chunks".to_string(),
                    input: InputMapping {
                        fields: btreemap([("chunks", "steps.chunk.chunks"), ("embeddings", "steps.embed_batch.embeddings")]),
                    },
                    when: None,
                },
            ],
            fuse: vec![],
            conditions: vec![],
            output: "cache_write".to_string(),
        },
    );

    pipelines.insert(
        "recall".to_string(),
        Pipeline {
            entry_point: "recall".to_string(),
            steps: vec![
                StepNode {
                    id: "embed".to_string(),
                    plugin: "gm".to_string(),
                    verb: "embed_query".to_string(),
                    input: InputMapping { fields: btreemap([("query", "request.query")]) },
                    when: None,
                },
                StepNode {
                    id: "search".to_string(),
                    plugin: "gm".to_string(),
                    verb: "search_with_recency".to_string(),
                    input: InputMapping {
                        fields: btreemap([("embedding", "steps.embed.embedding"), ("namespaces", "request.namespaces"), ("limit", "request.limit")]),
                    },
                    when: None,
                },
            ],
            fuse: vec![],
            conditions: vec![],
            output: "search".to_string(),
        },
    );

    DataflowDocument { schema_version: DATAFLOW_SCHEMA_VERSION, pipelines }
}

fn btreemap<const N: usize>(pairs: [(&str, &str); N]) -> std::collections::BTreeMap<String, Value> {
    pairs.into_iter().map(|(k, v)| (k.to_string(), Value::String(v.to_string()))).collect()
}
