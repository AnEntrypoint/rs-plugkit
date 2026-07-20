use serde::{Deserialize, Serialize};
use crate::pkfs;

/// One FSM node. `key` is the canonical uppercase phase identifier
/// (matches Phase::as_str()); `prose_key` is what instructions::get_instruction
/// passes to prose::resolve (today's compiled-in phase prose files keep their
/// existing lowercase keys -- plan/execute/emit/verify/consolidate/
/// update_docs/browser -- for backward compat with the shipped .md files, so
/// a project's custom phase names its own prose_key freely).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateNode {
    pub key: String,
    pub prose_key: String,
    #[serde(default)]
    pub skill: Option<String>,
}

/// One directed edge. `gates` names zero or more Gate.name entries (see
/// GateDef below) that must ALL pass before this edge may be taken; order
/// matters -- gates evaluate in list order and the first failure's message
/// is what the caller sees, matching today's hardcoded gates.rs sequencing
/// (residual-scan-fired checked before prd-open, etc).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub gates: Vec<String>,
}

/// A named, independently-evaluable gate condition. `predicate` is the
/// REGISTERED Rust predicate name (see predicate::evaluate) -- the actual
/// boolean check stays compiled, only WHICH predicates gate WHICH edge (and
/// in what order, with what message) is data. `hook` is an optional path to
/// a jit-executor script (relative to .gm/instructions/hooks/) that the
/// orchestrator runs via exec_js instead of (or in addition to, depending on
/// `hook_mode`) the built-in predicate, per fsm-framework-jit-hook-concreting
/// -- letting a project "concrete" its own custom condition without a Rust
/// rebuild. A hook script's `return` value (explicit `return`, required --
/// exec_js wraps every script in an async function body, so a bare trailing
/// statement is discarded, not an implicit return) is coerced to bool;
/// non-boolean/missing-return/throw = gate fails closed (deny), never open.
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
    /// No hook, or hook present but predicate is authoritative (hook result
    /// ignored) -- the default, byte-identical to a project with no hooks.
    #[default]
    PredicateOnly,
    /// Hook REPLACES the compiled predicate entirely.
    HookOnly,
    /// Both must pass (compiled predicate AND hook) -- lets a project add a
    /// stricter custom condition on top of a built-in one without losing
    /// the built-in's own safety check.
    Both,
}

/// Project-level policy knobs that were previously hardcoded Rust consts in
/// gates.rs (TOPLEVEL_DOC_ALLOWLIST, AWAIT_ALLOWED_VERBS, LONGGAP_EXEMPT,
/// GATE_REPEAT_ESCALATE_THRESHOLD, and the 300_000ms long-gap threshold
/// literal) -- each is a project-policy decision, not a code invariant, so
/// it belongs in the vendored/overridable graph, not a rebuild-to-change
/// const. `#[serde(default = "...")]` on every field means an OLDER vendored
/// graph.json (written before this field existed) still deserializes fine,
/// falling back to the same values gates.rs used to hardcode.
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

impl Default for Policy {
    fn default() -> Self {
        Policy {
            toplevel_doc_allowlist: default_toplevel_doc_allowlist(),
            await_allowed_verbs: default_await_allowed_verbs(),
            longgap_exempt_verbs: default_longgap_exempt_verbs(),
            gate_repeat_escalate_threshold: default_gate_repeat_escalate_threshold(),
            longgap_threshold_ms: default_longgap_threshold_ms(),
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

    /// The single outbound edge for a phase under today's linear-chain
    /// semantics (next_phase/next_skill both assume exactly one "forward"
    /// edge per phase, no branching) -- a project's custom graph CAN define
    /// multiple outbound edges from one state (branching), but the bare
    /// `transition` (no explicit `to`) call always takes the FIRST edge
    /// listed for the current phase, matching next_phase's old
    /// deterministic single-successor behavior. An explicit `transition
    /// {to:"X"}` bypasses this entirely and is validated against
    /// edge_between instead.
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

/// The default graph, hand-transcribed from the pre-existing hardcoded
/// transitions.rs::next_phase/next_skill and gates.rs's CONSOLIDATE/COMPLETE
/// gate checks, so a project with no .gm/instructions/fsm/ override behaves
/// byte-identically to the pre-dynamic-phase behavior. Prose keys match the
/// EXISTING compiled-in .md files (instructions::get_instruction's key
/// column) -- update_docs is COMPLETE's prose key, matching that pre-existing
/// (not-a-typo) naming.
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
            Edge { from: "VERIFY".into(), to: "CONSOLIDATE".into(), gates: vec!["residual-scan-fired".into(), "prd-all-closed".into(), "mutables-all-resolved".into()] },
            Edge { from: "CONSOLIDATE".into(), to: "COMPLETE".into(), gates: vec!["prd-all-closed".into(), "mutables-all-resolved".into(), "worktree-clean".into(), "residual-scan-fired".into(), "ci-validated-fresh".into(), "browser-witness-coverage".into()] },
            // COMPLETE has no default forward edge -- matches next_phase's
            // Phase::Complete => Phase::Complete self-loop (terminal, bare
            // `transition` with no target is a no-op there).
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
        ],
        policy: Policy::default(),
    }
}

const GRAPH_OVERRIDE_PATH: &str = ".gm/instructions/fsm/graph.json";

/// The active graph: a project's .gm/instructions/fsm/graph.json REPLACES
/// the built-in wholesale if present (not a per-field merge -- edges and
/// gates are interdependent, so a partial override risks referencing a gate
/// name or state that doesn't exist elsewhere in a half-overridden graph;
/// the scaffold verb's own default output is the safe starting point for
/// customization, not a diff against the compiled-in one). Falls back to
/// default_graph() when absent, unreadable, or fails to parse (a malformed
/// override graph is treated as no-override rather than a hard error, since
/// gm's own FSM dispatch loop must keep functioning even if a project's
/// customization attempt has a typo -- the malformed-graph condition itself
/// is worth surfacing, so a parse failure emits a deviation event).
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

/// The default graph serialized, for the scaffold verb to write out
/// byte-identically to what graph() would fall back to.
pub fn default_graph_json_pretty() -> String {
    serde_json::to_string_pretty(&default_graph()).unwrap_or_default()
}
