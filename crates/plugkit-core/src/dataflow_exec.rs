//! Executor for `dataflow::Pipeline`. Walks a resolved pipeline's steps and
//! fuse nodes in declaration order, building each step's request body from
//! its `InputMapping` (resolved against the original request and every prior
//! step/fuse-node's output so far), dispatching gm-internal verbs directly
//! (the same internal functions the pre-rewire fixed call sites already
//! called) and every other plugin through the real `host_plugin_call`
//! (`wasm_dispatch::host_abi::plugin_call`) -- there is no wasm self-call
//! back into gm's own dispatch table, since that would round-trip through
//! the host for a call already running inside the guest.

use serde_json::{json, Value};

use crate::dataflow::{FuseNode, InputMapping, Pipeline};

/// Every value a step or fuse node has produced so far, keyed by its id, plus
/// the original pipeline request body under the reserved key `""`.
pub struct RunState {
    outputs: std::collections::BTreeMap<String, Value>,
    request: Value,
}

impl RunState {
    pub fn new(request: Value) -> Self {
        RunState { outputs: std::collections::BTreeMap::new(), request }
    }

    fn resolve_ref(&self, ref_str: &str) -> Value {
        if let Some(rest) = ref_str.strip_prefix("literal:") {
            return Value::String(rest.to_string());
        }
        let (root, path) = match ref_str.split_once('.') {
            Some((r, p)) => (r, p),
            None => (ref_str, ""),
        };
        let base = match root {
            "request" => &self.request,
            "steps" => {
                let Some((step_id, rest)) = path.split_once('.') else { return Value::Null };
                let Some(v) = self.outputs.get(step_id) else { return Value::Null };
                return dig(v, rest);
            }
            _ => return Value::Null,
        };
        if path.is_empty() { base.clone() } else { dig(base, path) }
    }

    fn build_body(&self, mapping: &InputMapping) -> Value {
        let mut obj = serde_json::Map::new();
        for (key, v) in &mapping.fields {
            let resolved = match v {
                Value::String(s) => self.resolve_ref(s),
                other => other.clone(),
            };
            obj.insert(key.clone(), resolved);
        }
        Value::Object(obj)
    }

    fn eval_condition(&self, cond: &crate::dataflow::Condition) -> bool {
        let actual = self.resolve_ref(&format!("request.{}", cond.field));
        actual == cond.equals
    }
}

fn dig(v: &Value, dotted_path: &str) -> Value {
    if dotted_path.is_empty() {
        return v.clone();
    }
    let mut cur = v;
    for seg in dotted_path.split('.') {
        match cur.get(seg) {
            Some(next) => cur = next,
            None => return Value::Null,
        }
    }
    cur.clone()
}

/// Dispatch one step's plugin+verb, given its already-built request body.
/// gm-internal verbs call the real internal function directly (no host
/// round-trip); anything else routes through `plugin_call`.
fn dispatch_step(plugin: &str, verb: &str, body: &Value) -> Value {
    if plugin == "gm" {
        return dispatch_gm_internal(verb, body);
    }
    crate::wasm_dispatch::plugin_call(plugin, verb, body)
}

fn dispatch_gm_internal(verb: &str, body: &Value) -> Value {
    match verb {
        "embed_query" => {
            let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
            json!({ "embedding": crate::wasm_dispatch::embed_query(query) })
        }
        "vector_search" => {
            let embedding = body.get("embedding").and_then(|e| e.get("embedding")).cloned().unwrap_or(Value::Null);
            let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
            let k = body.get("k").and_then(|v| v.as_u64()).unwrap_or(8) as u32;
            crate::wasm_dispatch::vec_search_local(&embedding, ns, k)
        }
        "bm25_rank" => {
            let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let k = body.get("k").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
            let cfg = crate::ragconfig::RagConfig::resolved();
            let mut corpus = crate::code_index::FusionCorpus::load();
            let ids = corpus.bm25_rank_cfg(query, k, &cfg.scoring);
            json!({ "ids": ids })
        }
        "git_commit_rank" => {
            let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
            json!({ "commits": crate::code_index::git_commit_rank(query, limit) })
        }
        "search_with_recency" => {
            let embedding = body.get("embedding").and_then(|e| e.get("embedding")).cloned().unwrap_or(Value::Null);
            let namespaces: Vec<String> = body
                .get("namespaces")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(8) as usize;
            let now_ms = unsafe { crate::wasm_dispatch::host_now_ms() } as i64;
            crate::rssearch_vectors::search_with_recency(&embedding, &namespaces, limit, now_ms).unwrap_or(Value::Null)
        }
        "extract_chunks" => {
            let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let source = body.get("source").and_then(|v| v.as_str()).unwrap_or("");
            let lang = body.get("lang").and_then(|v| v.as_str()).unwrap_or("");
            let chunks = crate::code_index::extract_chunks(path, source, lang);
            let bodies: Vec<Value> = chunks.iter().map(|(_, _, _, _, body)| json!(body)).collect();
            let structured: Vec<Value> = chunks
                .iter()
                .map(|(kind, name, line_start, line_end, body)| {
                    json!({ "kind": kind, "name": name, "line_start": line_start, "line_end": line_end, "body": body })
                })
                .collect();
            json!({ "bodies": bodies, "chunks": structured })
        }
        other => json!({ "ok": false, "error": format!("unknown gm-internal dataflow verb: {other}") }),
    }
}

/// Run `pipeline` against `request`, returning the value named by its
/// `output` field. Steps run in declaration order (this executor does not
/// yet reorder for a genuinely independent-order DAG execution -- the
/// compiled defaults are ordered so today's fixed sequence is exactly
/// reproduced; a project's own pipeline is responsible for its own valid
/// step order until a topological scheduler lands as a later increment).
pub fn run(pipeline: &Pipeline, request: Value) -> Value {
    let mut state = RunState::new(request);
    for step in &pipeline.steps {
        if let Some(cond_name) = &step.when {
            let Some(cond) = pipeline.conditions.iter().find(|c| &c.name == cond_name) else { continue };
            if !state.eval_condition(cond) {
                continue;
            }
        }
        let body = state.build_body(&step.input);
        let out = dispatch_step(&step.plugin, &step.verb, &body);
        state.outputs.insert(step.id.clone(), out);
    }
    for fuse in &pipeline.fuse {
        let out = run_fuse(fuse, &state);
        state.outputs.insert(fuse.id.clone(), out);
    }
    state.outputs.get(&pipeline.output).cloned().unwrap_or(Value::Null)
}

fn run_fuse(fuse: &FuseNode, state: &RunState) -> Value {
    match fuse.strategy.as_str() {
        "rrf_fuse" => {
            let cfg = crate::ragconfig::RagConfig::resolved();
            let lists: Vec<Vec<String>> = fuse
                .sources
                .iter()
                .map(|src| {
                    state
                        .outputs
                        .get(src)
                        .and_then(|v| v.get("ids").or_else(|| v.get("hits")))
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default()
                })
                .collect();
            let weights: Vec<f64> = fuse
                .sources
                .iter()
                .enumerate()
                .map(|(i, _)| if i == 0 { 1.0 } else { cfg.scoring.fusion_identifier_boost })
                .collect();
            let query = state.request.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let fused = rs_search::fusion::fuse_n_cfg(&lists, &weights, query, cfg.scoring.fusion_rrf_k);
            json!({
                "ids": fused.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
                "scored": fused.into_iter().map(|(id, score)| json!({"id": id, "score": score})).collect::<Vec<_>>(),
            })
        }
        other => json!({ "ok": false, "error": format!("unknown fuse strategy: {other}") }),
    }
}
