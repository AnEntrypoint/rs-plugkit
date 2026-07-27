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
    /// Verbs whose dispatch RESETS the long-gap clock (`.gm/last-instruction-ts`).
    ///
    /// The third member of the same family as the two above, and the only one
    /// that was not configurable: exempt verbs are not PENALISED by the clock,
    /// whereas these actively REFRESH it. A project that renames or adds an
    /// orienting verb has to be able to say so here, or that verb dispatches
    /// forever without ever clearing the long-gap denial it keeps triggering.
    #[serde(default = "default_longgap_refresh_verbs")]
    pub longgap_refresh_verbs: Vec<String>,
    /// Verbs treated as "a shell" for the shell-bypass check, and the tool that
    /// bypass is steering people toward.
    ///
    /// Hardcoding these made the rule un-vendorable: a workflow whose shell verb
    /// is named something else got no protection at all, and one that
    /// legitimately wants shell git had no way to say so. Kept as policy rather
    /// than a compile-time constant precisely because "which verb is a shell" is
    /// a property of the workflow, not of the engine.
    #[serde(default = "default_shell_verbs")]
    pub shell_verbs: Vec<String>,
    /// Set false to allow shell git. Defaults to true: the shell path bypasses
    /// the porcelain gate and the witness ledger, so it stays denied unless a
    /// workflow deliberately opts out.
    #[serde(default = "default_deny_shell_git")]
    pub deny_shell_git: bool,
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
fn default_longgap_refresh_verbs() -> Vec<String> {
    ["instruction", "transition", "phase-status", "prd-add", "prd-resolve", "prd-list",
     "mutable-add", "mutable-resolve", "mutable-list"]
        .iter().map(|s| s.to_string()).collect()
}
fn default_shell_verbs() -> Vec<String> {
    // Superset of the previously-hardcoded list -- narrowing it would silently
    // drop protection on a shell this gate already covered.
    ["bash", "sh", "shell", "zsh", "powershell", "ps1", "pwsh", "cmd"].iter().map(|s| s.to_string()).collect()
}
fn default_deny_shell_git() -> bool { true }
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
            longgap_refresh_verbs: default_longgap_refresh_verbs(),
            shell_verbs: default_shell_verbs(),
            deny_shell_git: default_deny_shell_git(),
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

    /// Referential integrity of a loaded graph, checked BEFORE it is trusted.
    ///
    /// A vendored graph is remote-authored input, and every one of these
    /// mistakes fails silently at runtime rather than at load: an edge naming a
    /// state that does not exist is simply never traversable; an edge naming a
    /// gate that does not exist drops that gate's protection while the graph
    /// still reads as guarded; a `gates.predicate` outside the compiled
    /// registry produces a gate that can never be satisfied. Each is worse than
    /// a parse error because the config looks correct.
    ///
    /// Returns every problem rather than the first, so an author fixing a
    /// hand-written graph sees the whole list instead of peeling them off one
    /// dispatch at a time.
    /// Keys a graph may legitimately carry, for the non-fatal typo warning.
    ///
    /// Deliberately NOT enforced via serde's `deny_unknown_fields`, which would
    /// be actively harmful here: this config is repo-backed and auto-updating,
    /// so a graph regenerated by a NEWER binary carries fields an OLDER binary
    /// has never heard of, and a hard reject would drop every older binary in
    /// the fleet back to compiled defaults. Serde's default-on-missing gives
    /// backward compatibility; rejecting unknown keys would remove forward
    /// compatibility, and this system needs both. A warning gets the typo
    /// signal without the outage.
    const KNOWN_POLICY_KEYS: &'static [&'static str] = &[
        "toplevel_doc_allowlist", "await_allowed_verbs", "longgap_exempt_verbs", "longgap_refresh_verbs",
        "shell_verbs", "deny_shell_git", "gate_repeat_escalate_threshold",
        "longgap_threshold_ms", "require_witness_evidence", "prd_closed_statuses",
        "mutables_resolved_statuses", "reject_duplicate_witness", "initial_phase",
        "terminal_phase", "mutables_default_status", "mutables_witness_status",
        "mutables_require_witness_evidence", "cas_max_attempts",
    ];

    /// Report policy keys this build does not recognise, so a typo is visible
    /// rather than silently ignored. Never fatal -- see KNOWN_POLICY_KEYS.
    pub fn unknown_policy_keys(raw: &str) -> Vec<String> {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else { return Vec::new() };
        let Some(policy) = v.get("policy").and_then(|p| p.as_object()) else { return Vec::new() };
        policy
            .keys()
            .filter(|k| !Self::KNOWN_POLICY_KEYS.contains(&k.as_str()))
            .cloned()
            .collect()
    }

    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();

        if !self.has_state(&self.policy.initial_phase) {
            problems.push(format!("policy.initial_phase `{}` is not a declared state", self.policy.initial_phase));
        }
        if !self.has_state(&self.policy.terminal_phase) {
            problems.push(format!("policy.terminal_phase `{}` is not a declared state", self.policy.terminal_phase));
        }

        for e in &self.edges {
            if !self.has_state(&e.from) {
                problems.push(format!("edge `{} -> {}` starts at undeclared state `{}`", e.from, e.to, e.from));
            }
            if !self.has_state(&e.to) {
                problems.push(format!("edge `{} -> {}` ends at undeclared state `{}`", e.from, e.to, e.to));
            }
            for gate_name in &e.gates {
                if self.gate(gate_name).is_none() {
                    problems.push(format!(
                        "edge `{} -> {}` names gate `{}`, which is not defined in `gates` -- that edge would appear guarded while being unguarded",
                        e.from, e.to, gate_name
                    ));
                }
            }
        }

        for g in &self.gates {
            // A gate with neither a predicate nor a hook can never evaluate,
            // and per hook_mode's own semantics would deny forever.
            if g.predicate.is_none() && g.hook.is_none() {
                problems.push(format!("gate `{}` declares neither `predicate` nor `hook`, so it can never be satisfied", g.name));
            }
            if let Some(p) = &g.predicate {
                let known = crate::orchestrator::transitions::known_predicates();
                if !known.iter().any(|(n, _)| n == p) {
                    problems.push(format!(
                        "gate `{}` names predicate `{}`, which is not in the compiled registry ({}) -- this gate could never be satisfied",
                        g.name,
                        p,
                        known.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
                    ));
                }
            }
        }

        // A state with no compiled prose default AND no vendored .md silently serves
        // ENTRY prose (see instructions::compiled_default_for_prose_key's `_` arm).
        // That is a config typo that looks like a working phase, so it is worth naming
        // -- but it is NOT fatal, because the vendored file may legitimately be added
        // later, and serving entry prose is a degraded-but-working state, not a broken one.
        for s in &self.states {
            if crate::orchestrator::instructions::has_compiled_default_for_prose_key(&s.prose_key) {
                continue;
            }
            if prose_file_exists(&s.prose_key) {
                continue;
            }
            problems.push(format!(
                "state `{}` declares prose_key `{}`, which has neither a vendored .gm/instructions/{}.md nor a compiled default -- it will silently serve ENTRY prose",
                s.key, s.prose_key, s.prose_key
            ));
        }

        // Reachability: a state no edge leads to can never be entered (the initial
        // phase excepted, which is entered by definition rather than by an edge).
        for s in &self.states {
            if s.key == self.policy.initial_phase {
                continue;
            }
            if !self.edges.iter().any(|e| e.to == s.key) {
                problems.push(format!(
                    "state `{}` is unreachable -- no edge leads to it",
                    s.key
                ));
            }
        }

        // Liveness: every state must be able to reach the terminal phase, or a chain
        // entering it can never legally finish. Walked as a reverse BFS from terminal
        // over the edge set, so a multi-hop path counts as reachable.
        if self.has_state(&self.policy.terminal_phase) {
            let mut can_reach: Vec<&str> = vec![self.policy.terminal_phase.as_str()];
            loop {
                let before = can_reach.len();
                for e in &self.edges {
                    if can_reach.iter().any(|k| *k == e.to.as_str())
                        && !can_reach.iter().any(|k| *k == e.from.as_str())
                    {
                        can_reach.push(e.from.as_str());
                    }
                }
                if can_reach.len() == before {
                    break;
                }
            }
            for s in &self.states {
                if !can_reach.iter().any(|k| *k == s.key.as_str()) {
                    problems.push(format!(
                        "state `{}` has no path to terminal phase `{}` -- a chain entering it could never reach COMPLETE",
                        s.key, self.policy.terminal_phase
                    ));
                }
            }
        }

        problems
    }
}

#[cfg(target_arch = "wasm32")]
fn prose_file_exists(prose_key: &str) -> bool {
    crate::pkfs::read_to_string(&format!(".gm/instructions/{}.md", prose_key)).is_some()
}

#[cfg(not(target_arch = "wasm32"))]
fn prose_file_exists(prose_key: &str) -> bool {
    std::path::Path::new(&format!(".gm/instructions/{}.md", prose_key)).exists()
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
            Ok(g) => {
                // Parsing proves the SHAPE is right; it says nothing about
                // whether the graph refers to things that exist. Reject a
                // referentially-broken override the same way a malformed one is
                // rejected -- falling back to the compiled default -- because
                // running it would mean traversing edges to nowhere or trusting
                // gates that silently do not apply.
                // Non-fatal: an unrecognised policy key is far more likely a
                // typo (silently ignored, so the author thinks it took effect)
                // than a genuine forward-compat field -- but it can be either,
                // so it warns rather than rejects.
                let unknown = Graph::unknown_policy_keys(&raw);
                if !unknown.is_empty() {
                    #[cfg(target_arch = "wasm32")]
                    crate::wasm_dispatch::emit_event("fsm_graph_unknown_policy_keys", serde_json::json!({
                        "path": GRAPH_OVERRIDE_PATH,
                        "keys": unknown,
                        "reason": "these policy keys are not recognised by this build and are being IGNORED -- a typo would look exactly like this. If they are from a newer build, this is expected and harmless.",
                    }));
                }
                let problems = g.validate();
                if problems.is_empty() {
                    clear_graph_rejection();
                    g
                } else {
                    #[cfg(target_arch = "wasm32")]
                    crate::wasm_dispatch::emit_event("fsm_graph_override_invalid", serde_json::json!({
                        "path": GRAPH_OVERRIDE_PATH,
                        "problems": problems,
                        "reason": "graph parsed but failed referential-integrity validation; falling back to the built-in default this dispatch",
                    }));
                    record_graph_rejection("invalid", &problems.join("; "));
                    default_graph()
                }
            }
            Err(e) => {
                #[cfg(target_arch = "wasm32")]
                crate::wasm_dispatch::emit_event("fsm_graph_override_malformed", serde_json::json!({
                    "path": GRAPH_OVERRIDE_PATH,
                    "error": e.to_string(),
                    "reason": "falling back to the built-in default graph this dispatch",
                }));
                record_graph_rejection("malformed", &e.to_string());
                default_graph()
            }
        },
        None => {
            clear_graph_rejection();
            default_graph()
        }
    }
}

/// Where a graph rejection is recorded so it OUTLIVES the dispatch that hit it.
pub const GRAPH_REJECTION_PATH: &str = ".gm/fsm-graph-rejected.json";

/// Persist the fact that a vendored graph was rejected and the default is
/// serving in its place.
///
/// Until now the only signal was an emitted event, which is invisible on
/// native (the emit is cfg-gated to wasm32) and easy to miss even on wasm.
/// The operator's experience was gm behaving perfectly normally while their
/// entire config was ignored -- the worst shape a config failure can take,
/// because nothing about the running system looks wrong. A file on disk can
/// be read by the instruction payload, by a human, or by CI, long after the
/// dispatch that produced it.
fn record_graph_rejection(kind: &str, detail: &str) {
    let payload = serde_json::json!({
        "path": GRAPH_OVERRIDE_PATH,
        "kind": kind,
        "detail": detail,
        "effect": "the built-in default graph is serving; every customisation in this file is being IGNORED",
    });
    let _ = crate::pkfs::write(GRAPH_REJECTION_PATH, &payload.to_string());
}

/// Drop a stale rejection once the graph loads cleanly (or is removed), so the
/// marker always reflects the CURRENT state rather than the worst state ever
/// seen. A rejection notice that outlives the problem it describes trains
/// people to ignore it.
/// Truncated rather than deleted: pkfs exposes no remove, and lib.rs's own
/// clear_marker uses the same empty-write convention. `graph_rejection()`
/// treats empty as absent.
fn clear_graph_rejection() {
    if crate::pkfs::exists(GRAPH_REJECTION_PATH) {
        let _ = crate::pkfs::write(GRAPH_REJECTION_PATH, "");
    }
}

/// Gates the BUILT-IN default enforces on an edge that the ACTIVE graph does
/// not, per edge.
///
/// A vendored graph.json replaces the default wholesale -- there is no merge --
/// so a project that vendored before a gate was added never receives it, and
/// its edges stay permanently weaker than the built-in with nothing saying so.
/// `claim-audit-clean` and `submodules-clean` are exactly this case today.
///
/// Deliberately REPORTS rather than merges. Silently adding gates to a graph
/// someone wrote by hand would change their FSM's meaning under them, which is
/// its own failure; and a project may have dropped a gate on purpose. Naming
/// the difference lets that be a decision instead of an accident.
///
/// Returns `(from, to, missing_gates)` for each edge that is weaker than its
/// default counterpart. An edge the default does not have at all is not
/// reported: it is a genuinely new edge, not a weakened one.
pub fn gates_missing_vs_default(active: &Graph) -> Vec<(String, String, Vec<String>)> {
    let default = default_graph();
    let mut out = Vec::new();
    for de in &default.edges {
        let Some(ae) = active.edge_between(&de.from, &de.to) else { continue };
        let missing: Vec<String> = de
            .gates
            .iter()
            .filter(|g| !ae.gates.contains(g))
            .cloned()
            .collect();
        if !missing.is_empty() {
            out.push((de.from.clone(), de.to.clone(), missing));
        }
    }
    out
}

/// The active graph rejection, if any -- for the instruction payload.
pub fn graph_rejection() -> Option<serde_json::Value> {
    let raw = crate::pkfs::read_to_string(GRAPH_REJECTION_PATH)?;
    if raw.trim().is_empty() { return None; }
    serde_json::from_str(&raw).ok()
}

pub fn default_graph_json_pretty() -> String {
    serde_json::to_string_pretty(&default_graph()).unwrap_or_default()
}
