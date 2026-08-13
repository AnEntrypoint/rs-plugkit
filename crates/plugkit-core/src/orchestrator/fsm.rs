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
    #[serde(default)]
    pub next_dispatch: Option<String>,
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
    #[serde(default = "default_longgap_refresh_verbs")]
    pub longgap_refresh_verbs: Vec<String>,
    #[serde(default = "default_true")]
    pub fresh_prompt_resets_phase: bool,
    #[serde(default = "default_shell_verbs")]
    pub shell_verbs: Vec<String>,
    #[serde(default = "default_deny_shell_git")]
    pub deny_shell_git: bool,
    #[serde(default = "default_gate_repeat_escalate_threshold")]
    pub gate_repeat_escalate_threshold: u64,
    #[serde(default = "default_pseudo_phases")]
    pub pseudo_phases: Vec<(String, String)>,
    #[serde(default = "default_residual_checks_ordered_by_priority")]
    pub residual_checks: Vec<String>,
    #[serde(default = "default_long_gap_same_burst_ms")]
    pub long_gap_same_burst_ms: u64,
    #[serde(default = "default_long_gap_retry_bursts_before_escalate")]
    pub long_gap_retry_escalate_after: u32,
    #[serde(default = "default_hook_timeout_ms")]
    pub hook_timeout_ms: u64,
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
    #[serde(default = "default_deviation_severity")]
    pub deviation_severity: std::collections::BTreeMap<String, String>,
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
fn default_true() -> bool { true }
fn default_longgap_refresh_verbs() -> Vec<String> {
    ["instruction", "transition", "phase-status", "prd-add", "prd-resolve", "prd-list",
     "mutable-add", "mutable-resolve", "mutable-list"]
        .iter().map(|s| s.to_string()).collect()
}
fn default_shell_verbs() -> Vec<String> {
    ["bash", "sh", "shell", "zsh", "powershell", "ps1", "pwsh", "cmd"].iter().map(|s| s.to_string()).collect()
}
fn default_deny_shell_git() -> bool { true }
fn default_gate_repeat_escalate_threshold() -> u64 { 3 }
fn default_hook_timeout_ms() -> u64 { 15_000 }
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
fn default_pseudo_phases() -> Vec<(String, String)> {
    vec![
        ("ENTRY".to_string(), "entry".to_string()),
        ("ORCHESTRATOR".to_string(), "entry".to_string()),
        ("BROWSER".to_string(), "browser".to_string()),
    ]
}

fn default_residual_checks_ordered_by_priority() -> Vec<String> {
    vec!["prd-open".to_string(), "browser-open".to_string(), "tasks-running".to_string(), "dirty-tree".to_string()]
}

fn default_long_gap_same_burst_ms() -> u64 { 5_000 }
fn default_long_gap_retry_bursts_before_escalate() -> u32 { 2 }
fn default_terminal_phase() -> String { "COMPLETE".to_string() }
fn default_mutables_default_status() -> String { "unknown".to_string() }
fn default_mutables_witness_status() -> String { "witnessed".to_string() }
fn default_mutables_require_witness_evidence() -> bool { true }
fn default_cas_max_attempts() -> u32 { 5 }
fn default_deviation_severity() -> std::collections::BTreeMap<String, String> {
    std::collections::BTreeMap::new()
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            toplevel_doc_allowlist: default_toplevel_doc_allowlist(),
            await_allowed_verbs: default_await_allowed_verbs(),
            longgap_exempt_verbs: default_longgap_exempt_verbs(),
            longgap_refresh_verbs: default_longgap_refresh_verbs(),
            fresh_prompt_resets_phase: default_true(),
            shell_verbs: default_shell_verbs(),
            deny_shell_git: default_deny_shell_git(),
            gate_repeat_escalate_threshold: default_gate_repeat_escalate_threshold(),
            hook_timeout_ms: default_hook_timeout_ms(),
            longgap_threshold_ms: default_longgap_threshold_ms(),
            require_witness_evidence: default_require_witness_evidence(),
            prd_closed_statuses: default_prd_closed_statuses(),
            mutables_resolved_statuses: default_mutables_resolved_statuses(),
            reject_duplicate_witness: default_reject_duplicate_witness(),
            initial_phase: default_initial_phase(),
            pseudo_phases: default_pseudo_phases(),
            residual_checks: default_residual_checks_ordered_by_priority(),
            long_gap_same_burst_ms: default_long_gap_same_burst_ms(),
            long_gap_retry_escalate_after: default_long_gap_retry_bursts_before_escalate(),
            terminal_phase: default_terminal_phase(),
            mutables_default_status: default_mutables_default_status(),
            mutables_witness_status: default_mutables_witness_status(),
            mutables_require_witness_evidence: default_mutables_require_witness_evidence(),
            cas_max_attempts: default_cas_max_attempts(),
            deviation_severity: default_deviation_severity(),
        }
    }
}

pub const GRAPH_SCHEMA_VERSION: u32 = 2;

pub const GRAPH_SCHEMA_VERSION_LEGACY: u32 = 0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub min_plugkit_version: Option<String>,
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

    pub const KNOWN_POLICY_KEYS: &'static [&'static str] = &[
        "toplevel_doc_allowlist", "await_allowed_verbs", "longgap_exempt_verbs", "longgap_refresh_verbs", "fresh_prompt_resets_phase",
        "shell_verbs", "deny_shell_git", "gate_repeat_escalate_threshold", "hook_timeout_ms",
        "longgap_threshold_ms", "require_witness_evidence", "prd_closed_statuses",
        "mutables_resolved_statuses", "reject_duplicate_witness", "initial_phase",
        "terminal_phase", "mutables_default_status", "mutables_witness_status",
        "mutables_require_witness_evidence", "cas_max_attempts", "deviation_severity",
    ];

    pub fn min_plugkit_version_unmet(&self) -> Option<String> {
        let declared = self.min_plugkit_version.as_ref()?.trim().to_string();
        if declared.is_empty() {
            return None;
        }
        let running = env!("CARGO_PKG_VERSION");
        let parse = |s: &str| -> Option<Vec<u64>> {
            let core = s.split(['-', '+']).next().unwrap_or(s);
            let parts: Vec<u64> = core.split('.').map(|p| p.trim().parse::<u64>().ok()).collect::<Option<Vec<u64>>>()?;
            if parts.is_empty() { None } else { Some(parts) }
        };
        let (Some(want), Some(have)) = (parse(&declared), parse(running)) else {
            return Some(format!(
                "graph declares min_plugkit_version `{declared}` which is not a dotted numeric version, \
                 so it cannot be compared against this build's `{running}` -- the floor is being ignored \
                 rather than silently enforced"
            ));
        };
        let len = want.len().max(have.len());
        for i in 0..len {
            let w = want.get(i).copied().unwrap_or(0);
            let h = have.get(i).copied().unwrap_or(0);
            if h > w {
                return None;
            }
            if h < w {
                return Some(format!(
                    "graph declares min_plugkit_version `{declared}` but this build is `{running}` -- \
                     it was authored against a newer plugkit, so any predicate, policy key or gate added \
                     since `{running}` is absent here. An unknown predicate name denies its gate forever \
                     and an unknown policy key is ignored, both of which read as a legitimately-failing \
                     workflow rather than as a version mismatch"
                ));
            }
        }
        None
    }

    pub fn policy_default_drift() -> Vec<String> {
        let mut problems = Vec::new();
        let defaulted = Policy::default();
        let Ok(from_impl) = serde_json::to_value(&defaulted) else {
            problems.push("policy: Default impl does not serialize".to_string());
            return problems;
        };
        let Some(obj) = from_impl.as_object() else {
            problems.push("policy: serialized Default is not an object".to_string());
            return problems;
        };
        for key in obj.keys() {
            if !Self::KNOWN_POLICY_KEYS.contains(&key.as_str()) {
                problems.push(format!(
                    "policy key `{key}` exists on the struct but is absent from KNOWN_POLICY_KEYS -- \
                     a project setting it would be reported as unknown and its vendored value would \
                     still apply, so the warning would be wrong in both directions"
                ));
            }
        }
        for key in Self::KNOWN_POLICY_KEYS {
            if !obj.contains_key(*key) {
                problems.push(format!(
                    "policy key `{key}` is listed in KNOWN_POLICY_KEYS but no longer exists on the \
                     struct -- a project setting it gets silence instead of an unknown-key warning"
                ));
            }
        }
        let from_serde: Result<Policy, _> = serde_json::from_str("{}");
        match from_serde {
            Ok(empty) => {
                if let Ok(serde_value) = serde_json::to_value(&empty) {
                    if serde_value != from_impl {
                        let mut differing: Vec<String> = Vec::new();
                        if let Some(a) = serde_value.as_object() {
                            for (k, v) in obj {
                                if a.get(k) != Some(v) {
                                    differing.push(k.clone());
                                }
                            }
                        }
                        problems.push(format!(
                            "policy: Default::default() and the serde default path disagree on {} -- \
                             a project that omits these fields gets different values than one that \
                             vendors the baseline, which is a silent behaviour change riding along \
                             with a supposedly no-op extraction",
                            if differing.is_empty() { "at least one field".to_string() } else { differing.join(", ") }
                        ));
                    }
                }
            }
            Err(e) => problems.push(format!("policy: an empty object does not deserialize: {e}")),
        }
        problems
    }

    pub fn unknown_policy_keys(raw: &str) -> Vec<String> {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else { return Vec::new() };
        let Some(policy) = v.get("policy").and_then(|p| p.as_object()) else { return Vec::new() };
        policy
            .keys()
            .filter(|k| !Self::KNOWN_POLICY_KEYS.contains(&k.as_str()))
            .cloned()
            .collect()
    }

    pub fn deviation_severity_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        for unknown in super::deviations::unknown_severity_overrides(&self.policy.deviation_severity) {
            warnings.push(format!(
                "policy.deviation_severity names `{unknown}`, which is not a kind in the compiled deviation registry (see fsm/deviations.md for the valid set) -- this override configures nothing"
            ));
        }
        for bad in super::deviations::invalid_severity_values(&self.policy.deviation_severity) {
            warnings.push(format!(
                "policy.deviation_severity entry `{bad}` is not a valid severity -- only \"deny\" and \"log\" are accepted; the registry default applies instead"
            ));
        }
        warnings
    }

    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();

        if self.policy.prd_closed_statuses.is_empty() {
            problems.push(
                "policy.prd_closed_statuses is empty -- no status could ever count as closed, so `prd-all-closed` could never pass".to_string(),
            );
        }
        if self.policy.mutables_resolved_statuses.is_empty() {
            problems.push(
                "policy.mutables_resolved_statuses is empty -- no status could ever count as resolved, so `mutables-all-resolved` could never pass".to_string(),
            );
        }

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

        for g in &self.gates {
            if matches!(g.hook_mode, HookMode::PredicateOnly) {
                continue;
            }
            let Some(hook) = g.hook.as_deref() else {
                problems.push(format!(
                    "gate `{}` declares hook_mode `{:?}` but names no hook -- hooks fail CLOSED, so this gate could never be satisfied",
                    g.name, g.hook_mode
                ));
                continue;
            };
            if !hook_file_exists(hook) {
                problems.push(format!(
                    "gate `{}` names hook `{}`, which does not exist at .gm/instructions/hooks/{} -- a missing hook fails CLOSED, so this gate would deny forever",
                    g.name, hook, hook
                ));
            }
        }

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

#[cfg(target_arch = "wasm32")]
pub fn resolve_hook_path(hook: &str) -> Option<String> {
    let local = format!(".gm/instructions/hooks/{}", hook);
    if crate::pkfs::read_to_string(&local).is_some() {
        return Some(local);
    }
    let cache_base = crate::config::resolve().cache_dir?;
    let remote = format!("{}/hooks/{hook}", cache_base.trim_end_matches(['/', '\\']));
    if crate::pkfs::read_to_string(&remote).is_some() {
        return Some(remote);
    }
    None
}

#[cfg(not(target_arch = "wasm32"))]
pub fn resolve_hook_path(_hook: &str) -> Option<String> {
    None
}

fn hook_file_exists(hook: &str) -> bool {
    resolve_hook_path(hook).is_some()
}

fn default_graph() -> Graph {
    let mut policy = Policy::default();
    policy.initial_phase = "SPECIFY".into();
    Graph {
        schema_version: GRAPH_SCHEMA_VERSION,
        min_plugkit_version: None,
        states: vec![
            StateNode { key: "SPECIFY".into(), prose_key: "specify".into(), skill: Some("gm-prove".into()) },
            StateNode { key: "PROVE".into(), prose_key: "prove".into(), skill: Some("gm-emit".into()) },
            StateNode { key: "EMIT".into(), prose_key: "emit".into(), skill: Some("gm-state".into()) },
            StateNode { key: "STATE".into(), prose_key: "state".into(), skill: Some("gm-conc".into()) },
            StateNode { key: "CONC".into(), prose_key: "conc".into(), skill: Some("gm-sec".into()) },
            StateNode { key: "SEC".into(), prose_key: "sec".into(), skill: Some("gm-res".into()) },
            StateNode { key: "RES".into(), prose_key: "res".into(), skill: Some("gm-decide".into()) },
            StateNode { key: "DECIDE".into(), prose_key: "decide".into(), skill: Some("gm-complete".into()) },
            StateNode { key: "COMPLETE".into(), prose_key: "update_docs".into(), skill: Some("update-docs".into()) },
        ],
        edges: vec![
            Edge { from: "SPECIFY".into(), to: "PROVE".into(), gates: vec![] },
            Edge { from: "PROVE".into(), to: "EMIT".into(), gates: vec!["mutables-all-resolved".into()] },
            Edge { from: "EMIT".into(), to: "STATE".into(), gates: vec!["no-synthetic-test-files".into(), "no-graphical-symbols-in-diff".into(), "no-admit-deferral-markers".into()] },
            Edge { from: "STATE".into(), to: "CONC".into(), gates: vec!["idempotent-dispatch-replay-safe".into()] },
            Edge { from: "CONC".into(), to: "SEC".into(), gates: vec![] },
            Edge { from: "SEC".into(), to: "RES".into(), gates: vec!["no-secrets-in-diff".into()] },
            Edge { from: "RES".into(), to: "DECIDE".into(), gates: vec!["no-unchecked-panics-in-diff".into()] },
            Edge { from: "DECIDE".into(), to: "COMPLETE".into(), gates: vec!["prd-all-closed".into(), "mutables-all-resolved".into(), "worktree-clean".into(), "residual-scan-fired".into(), "ci-validated-fresh".into(), "browser-witness-coverage".into(), "app-loads-witnessed".into(), "submodules-clean".into(), "claim-audit-clean".into(), "no-hedge-language-in-diff".into()] },
            Edge { from: "PROVE".into(), to: "SPECIFY".into(), gates: vec![] },
            Edge { from: "EMIT".into(), to: "SPECIFY".into(), gates: vec![] },
            Edge { from: "STATE".into(), to: "EMIT".into(), gates: vec![] },
            Edge { from: "STATE".into(), to: "SPECIFY".into(), gates: vec![] },
            Edge { from: "CONC".into(), to: "STATE".into(), gates: vec![] },
            Edge { from: "CONC".into(), to: "EMIT".into(), gates: vec![] },
            Edge { from: "SEC".into(), to: "STATE".into(), gates: vec![] },
            Edge { from: "SEC".into(), to: "EMIT".into(), gates: vec![] },
            Edge { from: "RES".into(), to: "EMIT".into(), gates: vec![] },
            Edge { from: "RES".into(), to: "SPECIFY".into(), gates: vec![] },
            Edge { from: "DECIDE".into(), to: "SPECIFY".into(), gates: vec![] },
            Edge { from: "DECIDE".into(), to: "PROVE".into(), gates: vec![] },
            Edge { from: "COMPLETE".into(), to: "COMPLETE".into(), gates: vec![] },
        ],
        gates: vec![
            GateDef {
                name: "residual-scan-fired".into(),
                predicate: Some("residual-scan-fired".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: Some("residual-scan".into()),
                message: "transition rejected: residual-scan not fired in this stop window -- dispatch `residual-scan` before DECIDE -> COMPLETE.".into(),
            },
            GateDef {
                name: "prd-all-closed".into(),
                predicate: Some("prd-all-closed".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: Some("prd-resolve".into()),
                message: "transition rejected: PRD items still pending -- execute or remove them before transitioning.".into(),
            },
            GateDef {
                name: "mutables-all-resolved".into(),
                predicate: Some("mutables-all-resolved".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: Some("mutable-resolve".into()),
                message: "transition rejected: mutables still pending -- resolve them with witness_evidence before transitioning.".into(),
            },
            GateDef {
                name: "worktree-clean".into(),
                predicate: Some("worktree-clean".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: None,
                message: "transition rejected: worktree dirty -- commit or revert before declaring done; an unpushed delta is an unwitnessed slice.".into(),
            },
            GateDef {
                name: "ci-validated-fresh".into(),
                predicate: Some("ci-validated-fresh".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: None,
                message: "transition rejected: CI/CD validation not witnessed fresh -- .gm/exec-spool/.ci-validated missing, stale, or not matching current HEAD sha. Witness the pipeline green for the pushed HEAD, then fs_write .gm/exec-spool/.ci-validated with {\"head_sha\":\"<git rev-parse HEAD>\"} and re-attempt.".into(),
            },
            GateDef {
                name: "browser-witness-coverage".into(),
                predicate: Some("browser-witness-coverage".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: None,
                message: "transition rejected: client-edit-no-witness -- one or more client-side files edited this session lack a matching browser-witness. Dispatch `browser` to page.evaluate the invariant each edit establishes, then re-attempt.".into(),
            },
            GateDef {
                name: "app-loads-witnessed".into(),
                predicate: Some("app-loads-witnessed".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: None,
                message: "transition rejected: this project declares a browser entrypoint (.gm/browser-config.json present) but no same-turn `browser` dispatch recorded a healthy app-loads witness. Absence of file edits is never grounds to skip this -- a confirmation/audit turn asserting the app works is itself a claim, and that claim needs the same live witness a code-change turn needs. Dispatch `browser` against the real running app, confirm it loads with zero console/page errors, then re-attempt.".into(),
            },
            GateDef {
                name: "claim-audit-clean".into(),
                predicate: Some("claim-audit-clean".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: None,
                message: "transition rejected: claim-audit not fired in this stop window, or a prior fire found a stale claim -- dispatch `claim-audit` to scan AGENTS.md for shipped/validated/fixed claims referencing a commit hash and verify each hash actually exists in this repo's git log; resolve any stale finding before re-attempting.".into(),
            },
            GateDef {
                name: "submodules-clean".into(),
                predicate: Some("submodules-clean".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: None,
                message: "transition rejected: submodule pointer drift -- one or more of this repo's tracked submodule gitlinks no longer match that submodule's own real checked-out HEAD (dispatch `submodule-check` to see which paths and their recorded-vs-actual SHAs). `git add <drifted-path>` for each, then git_commit/git_finalize to update this repo's own pointer before re-attempting. A submodule directory with no `.git` of its own (never `git submodule update --init`'d) is not drift and is skipped automatically.".into(),
            },
            GateDef {
                name: "no-synthetic-test-files".into(),
                predicate: Some("no-synthetic-test-files".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: None,
                message: "transition rejected: standing test file(s) introduced in the working diff -- VERIFY doctrine forbids them; verification is a live exec_js/browser witness against real code, never a suite. Remove the file(s) and re-attempt with a live witness.".into(),
            },
            GateDef {
                name: "no-admit-deferral-markers".into(),
                predicate: Some("no-admit-deferral-markers".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: None,
                message: "transition rejected: an admit/deferral marker (TODO/FIXME/XXX/HACK/unimplemented!/todo!/'not (yet) implemented') landed in the working diff -- a marker stands in for a complete proof. Finish the work or remove the marker, then re-attempt.".into(),
            },
            GateDef {
                name: "no-secrets-in-diff".into(),
                predicate: Some("no-secrets-in-diff".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: None,
                message: "transition rejected: a line in the working diff matches a high-confidence secret shape (API key, private key header, inline-password connection string, bearer literal). Route the secret through an env var or secret store, never a tracked literal, then re-attempt.".into(),
            },
            GateDef {
                name: "no-unchecked-panics-in-diff".into(),
                predicate: Some("no-unchecked-panics-in-diff".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: None,
                message: "transition rejected: a new non-test line panics, throws, or unwraps with no visible handling -- the exception model requires every raised error handled or explicitly propagated, never left to crash uncaught. Propagate (Result/catch) or remove, then re-attempt.".into(),
            },
            GateDef {
                name: "no-hedge-language-in-diff".into(),
                predicate: Some("no-hedge-language-in-diff".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: None,
                message: "transition rejected: a hedge/deferral phrase in touched prose stands in for a decision ('todo later', 'in a future session', 'as a stopgap', 'good enough for now', 'left as an exercise', 'out of scope for this'). Commit to the real answer or remove the hedge, then re-attempt.".into(),
            },
            GateDef {
                name: "no-graphical-symbols-in-diff".into(),
                predicate: Some("no-graphical-symbols-in-diff".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: None,
                message: "transition rejected: a decorative non-ASCII glyph landed in tracked source/prose (arrow, box-drawing, star, bullet, check/cross, emoji). Convert to its plain-ASCII equivalent, then re-attempt.".into(),
            },
            GateDef {
                name: "idempotent-dispatch-replay-safe".into(),
                predicate: Some("idempotent-dispatch-replay-safe".into()),
                hook: None,
                hook_mode: HookMode::PredicateOnly,
                next_dispatch: None,
                message: "transition rejected: the same (id, hash) dispatch audit tuple was recorded with two different outcomes this stop window -- a replayed dispatch must reach the same result (f-compose-f-equals-f), never a second different mutation. Resolve the divergence, then re-attempt.".into(),
            },
        ],
        policy,
    }
}

const GRAPH_OVERRIDE_PATH: &str = ".gm/instructions/fsm/graph.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphTier {
    LocalOverride,
    SourceRepo,
    CompiledDefault,
}

impl GraphTier {
    pub fn as_str(self) -> &'static str {
        match self {
            GraphTier::LocalOverride => "local_override",
            GraphTier::SourceRepo => "source_repo",
            GraphTier::CompiledDefault => "compiled_default",
        }
    }

    fn may_execute_hooks(self) -> bool {
        matches!(self, GraphTier::LocalOverride | GraphTier::SourceRepo)
    }
}

#[cfg(target_arch = "wasm32")]
fn source_repo_graph_path() -> Option<String> {
    let resolved = crate::config::resolve();
    let rel = resolved
        .config
        .value
        .get("fsm")
        .and_then(|f| f.get("graph"))
        .and_then(|g| g.as_str())?;
    let rel = rel.trim().trim_matches('/');
    if rel.is_empty() {
        return None;
    }
    if let Err(reason) = crate::config_path::validate_source_path(rel) {
        crate::wasm_dispatch::emit_event("fsm_graph_source_path_rejected", serde_json::json!({
            "path": rel,
            "reason": reason,
            "detail": "the resolved config's `fsm.graph` pointer is not a safe relative path, so the repo-supplied graph was not read. A pointer that escapes the cache directory is refused rather than normalised.",
        }));
        return None;
    }
    let base = resolved.cache_dir?;
    Some(format!("{}/{rel}", base.trim_end_matches(['/', '\\'])))
}

#[cfg(not(target_arch = "wasm32"))]
fn source_repo_graph_path() -> Option<String> {
    None
}

pub fn graph() -> Graph {
    graph_detailed().0
}

pub fn graph_detailed() -> (Graph, GraphTier, String) {
    if let Some(raw) = pkfs::read_to_string(GRAPH_OVERRIDE_PATH) {
        return match load_tier(&raw, GRAPH_OVERRIDE_PATH, GraphTier::LocalOverride) {
            Some(g) => (g, GraphTier::LocalOverride, GRAPH_OVERRIDE_PATH.to_string()),
            None => (default_graph(), GraphTier::CompiledDefault, COMPILED_PATH.to_string()),
        };
    }

    if let Some(path) = source_repo_graph_path() {
        if let Some(raw) = pkfs::read_to_string(&path) {
            return match load_tier(&raw, &path, GraphTier::SourceRepo) {
                Some(g) => (g, GraphTier::SourceRepo, path),
                None => (default_graph(), GraphTier::CompiledDefault, COMPILED_PATH.to_string()),
            };
        }
    }

    clear_graph_rejection();
    (default_graph(), GraphTier::CompiledDefault, COMPILED_PATH.to_string())
}

const COMPILED_PATH: &str = "<compiled default>";

fn load_tier(raw: &str, path: &str, tier: GraphTier) -> Option<Graph> {
    match serde_json::from_str::<Graph>(raw) {
        Ok(mut g) => {
            let unknown = Graph::unknown_policy_keys(raw);
            if !unknown.is_empty() {
                #[cfg(target_arch = "wasm32")]
                crate::wasm_dispatch::emit_event("fsm_graph_unknown_policy_keys", serde_json::json!({
                    "path": path,
                    "tier": tier.as_str(),
                    "keys": unknown,
                    "reason": "these policy keys are not recognised by this build and are being IGNORED -- a typo would look exactly like this. If they are from a newer build, this is expected and harmless.",
                }));
            }

            let severity_warnings = g.deviation_severity_warnings();
            if !severity_warnings.is_empty() {
                #[cfg(target_arch = "wasm32")]
                crate::wasm_dispatch::emit_event("fsm_graph_deviation_severity_warnings", serde_json::json!({
                    "path": path,
                    "tier": tier.as_str(),
                    "warnings": severity_warnings,
                    "reason": "these policy.deviation_severity entries name an unknown deviation kind or an invalid severity value, and are being IGNORED -- the registry default applies for each. Non-fatal by design: the rest of this graph, including its other severity overrides, is serving normally.",
                }));
            }

            let refused = strip_untrusted_hooks(&mut g, tier);
            if !refused.is_empty() {
                #[cfg(target_arch = "wasm32")]
                crate::wasm_dispatch::emit_event("fsm_graph_remote_hook_refused", serde_json::json!({
                    "path": path,
                    "tier": tier.as_str(),
                    "gates": refused,
                    "reason": "a gate hook is arbitrary JS executed on this machine at every gate evaluation. This graph came from the compiled-default tier, which never carries hooks by construction -- if this event fires, something wrote a hook into a compiled default, which should not be possible. The gates keep their compiled predicates and are evaluated predicate-only.",
                }));
                record_graph_rejection_at(
                    path,
                    tier,
                    REFUSED_HOOK_KIND,
                    &format!(
                        "graph from tier `{}` at {} declared hooks on gate(s) {} -- the compiled-default tier never carries hooks, so this is unexpected; those gates now evaluate predicate-only",
                        tier.as_str(),
                        path,
                        refused.join(", ")
                    ),
                );
            }

            let problems = g.validate();
            if problems.is_empty() {
                if refused.is_empty() {
                    clear_graph_rejection();
                }
                Some(g)
            } else {
                #[cfg(target_arch = "wasm32")]
                crate::wasm_dispatch::emit_event("fsm_graph_override_invalid", serde_json::json!({
                    "path": path,
                    "tier": tier.as_str(),
                    "problems": problems,
                    "reason": "graph parsed but failed referential-integrity validation; falling back to the built-in default this dispatch",
                }));
                record_graph_rejection_at(path, tier, "invalid", &problems.join("; "));
                None
            }
        }
        Err(e) => {
            #[cfg(target_arch = "wasm32")]
            crate::wasm_dispatch::emit_event("fsm_graph_override_malformed", serde_json::json!({
                "path": path,
                "tier": tier.as_str(),
                "error": e.to_string(),
                "reason": "falling back to the built-in default graph this dispatch",
            }));
            record_graph_rejection_at(path, tier, "malformed", &e.to_string());
            None
        }
    }
}

fn strip_untrusted_hooks(g: &mut Graph, tier: GraphTier) -> Vec<String> {
    if tier.may_execute_hooks() {
        return Vec::new();
    }
    let mut refused = Vec::new();
    for gate in &mut g.gates {
        if gate.hook.is_none() && matches!(gate.hook_mode, HookMode::PredicateOnly) {
            continue;
        }
        if gate.hook.is_none() {
            gate.hook_mode = HookMode::PredicateOnly;
            continue;
        }
        refused.push(gate.name.clone());
        gate.hook = None;
        gate.hook_mode = HookMode::PredicateOnly;
        if gate.predicate.is_none() {
            gate.predicate = Some(REFUSED_HOOK_PREDICATE.to_string());
        }
    }
    refused
}

const REFUSED_HOOK_PREDICATE: &str = "remote-hook-refused";

const REFUSED_HOOK_KIND: &str = "remote-hook-refused";

pub const GRAPH_REJECTION_PATH: &str = ".gm/fsm-graph-rejected.json";

fn record_graph_rejection_at(path: &str, tier: GraphTier, kind: &str, detail: &str) {
    let effect = match (kind, tier) {
        (REFUSED_HOOK_KIND, _) => "the graph itself IS serving -- only its hooks were refused. The affected gates now evaluate predicate-only, and any gate whose hook was its ONLY condition can no longer pass at all. Vendor the graph and its hook into .gm/instructions/fsm/graph.json to restore them.",
        (_, GraphTier::SourceRepo) => "the built-in default graph is serving; every customisation in the config repo's graph is being IGNORED. This file is a fetched cache artifact -- fix it in the config REPO, not here, or a refresh will overwrite the edit.",
        _ => "the built-in default graph is serving; every customisation in this file is being IGNORED",
    };
    let payload = serde_json::json!({
        "path": path,
        "tier": tier.as_str(),
        "kind": kind,
        "detail": detail,
        "effect": effect,
    });
    let _ = crate::pkfs::write(GRAPH_REJECTION_PATH, &payload.to_string());
}

fn clear_graph_rejection() {
    if crate::pkfs::exists(GRAPH_REJECTION_PATH) {
        let _ = crate::pkfs::write(GRAPH_REJECTION_PATH, "");
    }
}

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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StalenessReport {
    pub vendored_version: u32,
    pub current_version: u32,
    pub stale: bool,
    pub missing_states: Vec<String>,
    pub missing_edges: Vec<String>,
    pub missing_gates: Vec<String>,
    pub weakened_edges: Vec<String>,
    pub missing_policy_keys: Vec<String>,
    pub unknown_predicates: Vec<String>,
}

impl StalenessReport {
    pub fn has_findings(&self) -> bool {
        self.stale
            || !self.missing_states.is_empty()
            || !self.missing_edges.is_empty()
            || !self.missing_gates.is_empty()
            || !self.weakened_edges.is_empty()
            || !self.missing_policy_keys.is_empty()
            || !self.unknown_predicates.is_empty()
    }

    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.stale {
            out.push(format!(
                "vendored graph declares schema_version {} but this build emits {} -- it was vendored before the items below existed and has never been refreshed",
                self.vendored_version, self.current_version
            ));
        }
        for s in &self.missing_states {
            out.push(format!(
                "state `{s}` exists in this build's default graph but not in the vendored file -- any transition targeting it is refused as an undeclared phase"
            ));
        }
        for e in &self.missing_edges {
            out.push(format!(
                "edge `{e}` exists in this build's default graph but not in the vendored file -- that transition is not traversable at all"
            ));
        }
        for g in &self.missing_gates {
            out.push(format!(
                "gate `{g}` is defined in this build's default graph but not in the vendored file -- the condition it enforces is not being checked anywhere"
            ));
        }
        for w in &self.weakened_edges {
            out.push(format!(
                "edge {w} -- these gates guard that transition in the default and do NOT guard it here, so it passes unchecked"
            ));
        }
        for k in &self.missing_policy_keys {
            out.push(format!(
                "policy key `{k}` is absent from the vendored file -- the compiled default value applies, which is safe but means a later default change moves silently under this project"
            ));
        }
        for p in &self.unknown_predicates {
            out.push(format!(
                "gate predicate `{p}` in the vendored file is not in this build's compiled registry -- that gate can never be satisfied on this binary"
            ));
        }
        out
    }
}

pub fn staleness_report(active: &Graph, raw: Option<&str>) -> StalenessReport {
    let default = default_graph();

    let missing_states: Vec<String> = default
        .states
        .iter()
        .filter(|s| !active.has_state(&s.key))
        .map(|s| s.key.clone())
        .collect();

    let missing_edges: Vec<String> = default
        .edges
        .iter()
        .filter(|e| active.edge_between(&e.from, &e.to).is_none())
        .map(|e| format!("{} -> {}", e.from, e.to))
        .collect();

    let missing_gates: Vec<String> = default
        .gates
        .iter()
        .filter(|g| active.gate(&g.name).is_none())
        .map(|g| g.name.clone())
        .collect();

    let weakened_edges: Vec<String> = gates_missing_vs_default(active)
        .into_iter()
        .map(|(from, to, missing)| format!("`{} -> {}` is missing gate(s) {}", from, to, missing.join(", ")))
        .collect();

    let missing_policy_keys = match raw {
        Some(r) => absent_policy_keys(r),
        None => Vec::new(),
    };

    let known = crate::orchestrator::transitions::known_predicates();
    let unknown_predicates: Vec<String> = active
        .gates
        .iter()
        .filter_map(|g| g.predicate.clone())
        .filter(|p| !known.iter().any(|(n, _)| n == p))
        .collect();

    StalenessReport {
        vendored_version: active.schema_version,
        current_version: GRAPH_SCHEMA_VERSION,
        stale: active.schema_version < GRAPH_SCHEMA_VERSION,
        missing_states,
        missing_edges,
        missing_gates,
        weakened_edges,
        missing_policy_keys,
        unknown_predicates,
    }
}

fn absent_policy_keys(raw: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else { return Vec::new() };
    let Some(policy) = v.get("policy").and_then(|p| p.as_object()) else {
        return Graph::KNOWN_POLICY_KEYS.iter().map(|k| k.to_string()).collect();
    };
    Graph::KNOWN_POLICY_KEYS
        .iter()
        .filter(|k| !policy.contains_key(**k))
        .map(|k| k.to_string())
        .collect()
}

pub fn vendored_graph_raw() -> Option<String> {
    pkfs::read_to_string(GRAPH_OVERRIDE_PATH)
}

pub fn graph_rejection() -> Option<serde_json::Value> {
    let raw = crate::pkfs::read_to_string(GRAPH_REJECTION_PATH)?;
    if raw.trim().is_empty() { return None; }
    serde_json::from_str(&raw).ok()
}

pub fn default_graph_json_pretty() -> String {
    serde_json::to_string_pretty(&default_graph()).unwrap_or_default()
}
