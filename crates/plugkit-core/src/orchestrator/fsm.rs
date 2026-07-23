use serde::{Deserialize, Serialize};
use crate::pkfs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateNode {
    pub key: String,
    pub prose_key: String,
    #[serde(default)]
    pub skill: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub gates: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateDef {
    pub name: String,
    #[serde(default)]
    pub predicate: Option<String>,
    #[serde(default)]
    pub hook: Option<String>,
    #[serde(default)]
    pub hook_mode: HookMode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum HookMode {
    #[default]
    PredicateOnly,
    HookOnly,
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default = "default_toplevel_doc_allowlist")]
    pub toplevel_doc_allowlist: Vec<String>,
    #[serde(default = "default_await_allowed_verbs")]
    pub await_allowed_verbs: Vec<String>,
    #[serde(default = "default_longgap_exempt_verbs")]
    pub longgap_exempt_verbs: Vec<String>,
    #[serde(default = "default_gate_repeat_escalate_threshold")]
    pub gate_repeat_escalate_threshold: u64,
    #[serde(default = "default_longgap_threshold_ms")]
    pub longgap_threshold_ms: u64,
    #[serde(default = "default_require_witness_evidence")]
    pub require_witness_evidence: bool,
    #[serde(default = "default_prd_closed_statuses")]
    pub prd_closed_statuses: Vec<String>,
    #[serde(default = "default_mutables_resolved_statuses")]
    pub mutables_resolved_statuses: Vec<String>,
    #[serde(default = "default_reject_duplicate_witness")]
    pub reject_duplicate_witness: bool,
    #[serde(default = "default_initial_phase")]
    pub initial_phase: String,
    #[serde(default = "default_terminal_phase")]
    pub terminal_phase: String,
    #[serde(default = "default_mutables_default_status")]
    pub mutables_default_status: String,
    #[serde(default = "default_mutables_witness_status")]
    pub mutables_witness_status: String,
    #[serde(default = "default_mutables_require_witness_evidence")]
    pub mutables_require_witness_evidence: bool,
    #[serde(default = "default_cas_max_attempts")]
    pub cas_max_attempts: u32,
}

fn default_toplevel_doc_allowlist() -> Vec<String> {
    ["AGENTS.md", "CLAUDE.md", "README.md", "SKILLS.md", "CHANGELOG.md", "LICENSE", "LICENSE.md"]
        .iter().map(|s| s.to_string()).collect()
}
fn default_await_allowed_verbs() -> Vec<String> {
    ["memorize-continue", "instruction", "phase-status", "health"].iter().map(|s| s.to_string()).collect()
}
fn default_longgap_exempt_verbs() -> Vec<String> {
    ["health", "auto-recall", "wait", "sleep"].iter().map(|s| s.to_string()).collect()
}
fn default_gate_repeat_escalate_threshold() -> u64 { 3 }
fn default_longgap_threshold_ms() -> u64 { 300_000 }
fn default_require_witness_evidence() -> bool { true }
fn default_prd_closed_statuses() -> Vec<String> {
    ["done", "complete", "completed"].iter().map(|s| s.to_string()).collect()
}
fn default_mutables_resolved_statuses() -> Vec<String> {
    ["witnessed", "resolved"].iter().map(|s| s.to_string()).collect()
}
fn default_reject_duplicate_witness() -> bool { true }
fn default_initial_phase() -> String { "PLAN".to_string() }
fn default_terminal_phase() -> String { "COMPLETE".to_string() }
fn default_mutables_default_status() -> String { "unknown".to_string() }
fn default_mutables_witness_status() -> String { "witnessed".to_string() }
fn default_mutables_require_witness_evidence() -> bool { true }
fn default_cas_max_attempts() -> u32 { 5 }

impl Default for Policy {
    fn default() -> Self {
        Policy {
            toplevel_doc_allowlist: default_toplevel_doc_allowlist(),
            await_allowed_verbs: default_await_allowed_verbs(),
            longgap_exempt_verbs: default_longgap_exempt_verbs(),
            gate_repeat_escalate_threshold: default_gate_repeat_escalate_threshold(),
            longgap_threshold_ms: default_longgap_threshold_ms(),
            require_witness_evidence: default_require_witness_evidence(),
            prd_closed_statuses: default_prd_closed_statuses(),
            mutables_resolved_statuses: default_mutables_resolved_statuses(),
            reject_duplicate_witness: default_reject_duplicate_witness(),
            initial_phase: default_initial_phase(),
            terminal_phase: default_terminal_phase(),
            mutables_default_status: default_mutables_default_status(),
            mutables_witness_status: default_mutables_witness_status(),
            mutables_require_witness_evidence: default_mutables_require_witness_evidence(),
            cas_max_attempts: default_cas_max_attempts(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    pub states: Vec<StateNode>,
    pub edges: Vec<Edge>,
    #[serde(default)]
    pub gates: Vec<GateDef>,
    #[serde(default)]
    pub policy: Policy,
}

impl Graph {
    pub fn state(&self, key: &str) -> Option<&StateNode> {
        self.states.iter().find(|s| s.key.eq_ignore_ascii_case(key))
    }

    pub fn has_state(&self, key: &str) -> bool {
        self.state(key).is_some()
    }

    pub fn default_edge_from(&self, from: &str) -> Option<&Edge> {
        self.edges.iter().find(|e| e.from.eq_ignore_ascii_case(from))
    }

    pub fn edge_between(&self, from: &str, to: &str) -> Option<&Edge> {
        self.edges.iter().find(|e| e.from.eq_ignore_ascii_case(from) && e.to.eq_ignore_ascii_case(to))
    }

    pub fn gate(&self, name: &str) -> Option<&GateDef> {
        self.gates.iter().find(|g| g.name.eq_ignore_ascii_case(name))
    }
}

fn default_graph() -> Graph {
    Graph {
        states: vec![
            StateNode { key: "PLAN".into(), prose_key: "plan".into(), skill: Some("gm-execute".into()) },
            StateNode { key: "EXECUTE".into(), prose_key: "execute".into(), skill: Some("gm-emit".into()) },
            StateNode { key: "EMIT".into(), prose_key: "emit".into(), skill: Some("gm-verify".into()) },
            StateNode { key: "VERIFY".into(), prose_key: "verify".into(), skill: Some("gm-consolidate".into()) },
            StateNode { key: "CONSOLIDATE".into(), prose_key: "consolidate".into(), skill: Some("gm-complete".into()) },
            StateNode { key: "COMPLETE".into(), prose_key: "update_docs".into(), skill: Some("update-docs".into()) },
        ],
        edges: vec![
            Edge { from: "PLAN".into(), to: "EXECUTE".into(), gates: vec![] },
            Edge { from: "EXECUTE".into(), to: "EMIT".into(), gates: vec![] },
            Edge { from: "EMIT".into(), to: "VERIFY".into(), gates: vec![] },
            Edge { from: "VERIFY".into(), to: "CONSOLIDATE".into(), gates: vec!["residual-scan-fired".into(), "prd-all-closed".into(), "mutables-all-resolved".into(), "claim-audit-clean".into(), "submodules-clean".into()] },
            Edge { from: "EXECUTE".into(), to: "PLAN".into(), gates: vec![] },
            Edge { from: "EMIT".into(), to: "PLAN".into(), gates: vec![] },
            Edge { from: "VERIFY".into(), to: "PLAN".into(), gates: vec![] },
            Edge { from: "CONSOLIDATE".into(), to: "COMPLETE".into(), gates: vec!["prd-all-closed".into(), "mutables-all-resolved".into(), "worktree-clean".into(), "residual-scan-fired".into(), "ci-validated-fresh".into(), "browser-witness-coverage".into(), "submodules-clean".into()] },
            Edge { from: "COMPLETE".into(), to: "COMPLETE".into(), gates: vec![] },
        ],
        gates: vec![
            GateDef {
                name: "residual-scan-fired".into(),
                predicate: Some("residual-scan-fired".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                message: "transition to CONSOLIDATE rejected: residual-scan not fired in this stop window -- dispatch `residual-scan` before CONSOLIDATE.".into(),
            },
            GateDef {
                name: "prd-all-closed".into(),
                predicate: Some("prd-all-closed".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                message: "transition rejected: PRD items still pending -- execute or remove them before transitioning.".into(),
            },
            GateDef {
                name: "mutables-all-resolved".into(),
                predicate: Some("mutables-all-resolved".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                message: "transition rejected: mutables still pending -- resolve them with witness_evidence before transitioning.".into(),
            },
            GateDef {
                name: "worktree-clean".into(),
                predicate: Some("worktree-clean".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                message: "transition rejected: worktree dirty -- commit or revert before declaring done; an unpushed delta is an unwitnessed slice.".into(),
            },
            GateDef {
                name: "ci-validated-fresh".into(),
                predicate: Some("ci-validated-fresh".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                message: "transition rejected: CI/CD validation not witnessed fresh -- .gm/exec-spool/.ci-validated missing, stale, or not matching current HEAD sha. Witness the pipeline green for the pushed HEAD, then fs_write .gm/exec-spool/.ci-validated with {\"head_sha\":\"<git rev-parse HEAD>\"} and re-attempt.".into(),
            },
            GateDef {
                name: "browser-witness-coverage".into(),
                predicate: Some("browser-witness-coverage".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                message: "transition rejected: client-edit-no-witness -- one or more client-side files edited this session lack a matching browser-witness. Dispatch `browser` to page.evaluate the invariant each edit establishes, then re-attempt.".into(),
            },
            GateDef {
                name: "claim-audit-clean".into(),
                predicate: Some("claim-audit-clean".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                message: "transition to CONSOLIDATE rejected: claim-audit not fired in this stop window, or a prior fire found a stale claim -- dispatch `claim-audit` to scan AGENTS.md for shipped/validated/fixed claims referencing a commit hash and verify each hash actually exists in this repo's git log; resolve any stale finding before re-attempting.".into(),
            },
            GateDef {
                name: "submodules-clean".into(),
                predicate: Some("submodules-clean".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                message: "transition rejected: submodule pointer drift -- one or more of gm's tracked submodule gitlinks (agentplug, rs-plugkit, rs-codeinsight, rs-search, agentplug-bert, agentplug-libsql, agentplug-treesitter) no longer match that submodule's own real HEAD. `git add <drifted-path>` for each, then git_commit/git_finalize to update gm's own pointer before re-attempting.".into(),
            },
        ],
        policy: Policy::default(),
    }
}

const GRAPH_OVERRIDE_PATH: &str = ".gm/instructions/fsm/graph.json";

pub fn graph() -> Graph {
    match pkfs::read_to_string(GRAPH_OVERRIDE_PATH) {
        Some(raw) => match serde_json::from_str::<Graph>(&raw) {
            Ok(g) => g,
            Err(e) => {
                #[cfg(target_arch = "wasm32")]
                crate::wasm_dispatch::emit_event("fsm_graph_override_malformed", serde_json::json!({
                    "path": GRAPH_OVERRIDE_PATH,
                    "error": e.to_string(),
                    "reason": "falling back to the built-in default graph this dispatch",
                }));
                default_graph()
            }
        },
        None => default_graph(),
    }
}

pub fn default_graph_json_pretty() -> String {
    serde_json::to_string_pretty(&default_graph()).unwrap_or_default()
}
