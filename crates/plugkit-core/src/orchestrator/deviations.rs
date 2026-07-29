use serde::{Deserialize, Serialize};

/// What a deviation kind does when it fires.
///
/// This is the facet that used to be purely STRUCTURAL: a kind "denied" because
/// the branch that emitted it happened to `return GateVerdict::deny(..)` or
/// `return (body, err, 1)`, and "logged" because the branch happened to fall
/// through. Nothing declared the intent, so the only way to answer "does
/// unsolicited-doc-created block me?" was to read the control flow around each
/// emitter. Naming the severity makes it a fact the registry states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The emitter refuses the dispatch: a gate denial, or a non-zero rc.
    Deny,
    /// The emitter records the event and lets the dispatch proceed.
    Log,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Deny => "deny",
            Severity::Log => "log",
        }
    }

    pub fn parse(s: &str) -> Option<Severity> {
        match s.trim().to_ascii_lowercase().as_str() {
            "deny" => Some(Severity::Deny),
            "log" => Some(Severity::Log),
            _ => None,
        }
    }
}

/// The ONE table: name, human description, and default severity declared together.
///
/// Copied deliberately from `transitions.rs::predicate_table()`, and for the same
/// reason. Before this, ~21 deviation kinds existed only as inline `&str` literals
/// scattered across gates.rs, prd.rs, residual.rs, instructions/mod.rs, lib.rs,
/// poll_detect.rs and wasm_dispatch/events.rs, with no enumeration anywhere: no
/// operator could discover the set without grepping Rust, nothing could generate a
/// reference from it, and a typo in an emitted name was indistinguishable from a
/// new kind. Declaring name, description and severity in one place makes the set
/// enumerable, generates `.gm/instructions/fsm/deviations.md` from the same data the
/// emitters use, and gives `policy.deviation_severity` a key space to validate against.
///
/// Adding a kind means adding a row here. `kind_is_known` is how an emitter proves
/// its literal is real; `deviation_table` is what the vendor reference is generated
/// from. Neither can be satisfied by a name that is not in this table.
pub const DEVIATION_TABLE: &[(&str, &str, Severity)] = &[
    (
        "await-result-violation",
        "a verb outside `policy.await_allowed_verbs` was dispatched while a pending_step was in flight -- the pipeline is suspended and only memorize-continue advances it",
        Severity::Deny,
    ),
    (
        "bash-git-bypass",
        "a shell verb (per `policy.shell_verbs`) invoked `git` directly, bypassing the porcelain gate and the witness ledger -- the git_* verbs are the admissible surface",
        Severity::Deny,
    ),
    (
        "long-gap-no-instruction",
        "a verb was dispatched after more than `policy.longgap_threshold_ms` of idle with no intervening `instruction` -- idle mid-chain loses the recovery prose",
        Severity::Deny,
    ),
    (
        "long-gap-retry-without-instruction",
        "the same verb was retried after the long-gap gate already denied it, instead of dispatching the `instruction` its next_dispatch named",
        Severity::Deny,
    ),
    (
        "gate-deny",
        "a `transition` was refused because the destination edge's gates reported residuals -- the residual list names what is still open",
        Severity::Deny,
    ),
    (
        "stuck-loop-escalation",
        "the same gate denial has now fired `policy.gate_repeat_escalate_threshold` times in a row with no successful transition between attempts -- blind retry is not clearing it",
        Severity::Deny,
    ),
    (
        "unsolicited-doc-created",
        "an fs_write created a top-level .md/.txt outside `policy.toplevel_doc_allowlist` -- a report/summary file written instead of doing or witnessing the work",
        Severity::Log,
    ),
    (
        "prd-anti-shape",
        "a PRD row was carried into a closing transition already marked closed but with empty witness_evidence -- closed-without-evidence is the rubber-stamp shape",
        Severity::Log,
    ),
    (
        "prd-add-no-id",
        "a `prd-add` body arrived with no usable id, so one was derived from the subject -- the derived id is what `prd-resolve` must later reference",
        Severity::Log,
    ),
    (
        "prd-resolve-no-witness",
        "`prd-resolve` was dispatched with empty witness_evidence while `policy.require_witness_evidence` is set -- a row cannot close without evidence the work is real",
        Severity::Deny,
    ),
    (
        "prd-resolve-duplicate-witness",
        "`prd-resolve` supplied witness_evidence byte-identical to another row's, while `policy.reject_duplicate_witness` is set -- copy-pasted witness text across distinct rows is the rubber-stamp tell",
        Severity::Deny,
    ),
    (
        "prd-resolve-unknown-id",
        "`prd-resolve` named an id that is not in .gm/prd.yml -- the row was never prd-added in this chain, or the id is a typo (see `suggested_id`)",
        Severity::Deny,
    ),
    (
        "residual-premature",
        "`residual-scan` was dispatched while .gm/prd.yml still carries open rows -- the scan is a close-out probe and has nothing to report until the PRD is empty",
        Severity::Log,
    ),
    (
        "residual-dirty-tree",
        "`residual-scan` found an uncommitted/untracked delta in the worktree -- every porcelain entry needs triage (commit, gitignore, or revert) before close-out",
        Severity::Log,
    ),
    (
        "platform-search-drift",
        "a platform Grep/Glob fired during an in-flight chain -- codesearch/recall are the discovery surfaces; platform search is exploration outside the spool",
        Severity::Log,
    ),
    (
        "spool-poll",
        "a shell command was observed polling the exec-spool directly (ls/cat/sleep loop over .gm/exec-spool) -- results arrive by dispatch, polling is idle-mid-chain",
        Severity::Log,
    ),
    (
        "complete-chain-poll",
        "`instruction` was re-dispatched on an already-terminal chain with zero pending PRD rows and no fresh prompt -- the chain is closed; a new request resets it",
        Severity::Log,
    ),
    (
        "browser-witness-missing",
        "a client-side file was edited this session but never witnessed in a browser dispatch -- disk-Read is necessary and insufficient, the live page is the authority",
        Severity::Deny,
    ),
    (
        "browser-witness-hash-mismatch",
        "a client-side file was witnessed in the browser, then edited again -- the recorded witness hash no longer matches the file's current content",
        Severity::Deny,
    ),
    (
        "synthetic-test-file",
        "the working tree carries a standing test file (a `*.test.*`/`*.spec.*` path, or a `test/`/`__tests__/`/`spec/` directory) -- doctrine is live exec_js/browser witnesses, not framework legwork deferred to a later run",
        Severity::Log,
    ),
    (
        "push-non-main-branch",
        "`git_push` ran against a branch other than the repo's main line -- the workflow is main-only, a feature branch strands the slice",
        Severity::Log,
    ),
    (
        "push-dirty",
        "`git_push` was attempted with a dirty worktree -- a dirty-tree push advances an unwitnessed slice",
        Severity::Log,
    ),
    (
        "push-rebase-conflict",
        "`git_push`'s rebase-retry hit a conflict against the remote -- the push did not land and the conflict needs resolving first",
        Severity::Log,
    ),
    (
        "push-remote-outpaces",
        "`git_push` found the remote ahead after its rebase-retry budget was spent -- another writer is pushing to the same branch concurrently",
        Severity::Log,
    ),
    (
        "push-claimed-success-unverified",
        "`git_push` exited success but a post-push fetch found origin does not match local HEAD -- the push did not actually land despite the reported exit code",
        Severity::Deny,
    ),
];

/// Every registered kind with its description, for generated references.
pub fn known_deviations() -> Vec<(&'static str, &'static str, Severity)> {
    DEVIATION_TABLE.to_vec()
}

pub fn kind_is_known(kind: &str) -> bool {
    DEVIATION_TABLE.iter().any(|(name, _, _)| *name == kind)
}

pub fn default_severity(kind: &str) -> Option<Severity> {
    DEVIATION_TABLE
        .iter()
        .find(|(name, _, _)| *name == kind)
        .map(|(_, _, sev)| *sev)
}

pub fn description(kind: &str) -> Option<&'static str> {
    DEVIATION_TABLE
        .iter()
        .find(|(name, _, _)| *name == kind)
        .map(|(_, desc, _)| *desc)
}

/// The effective severity of a kind, after `policy.deviation_severity` overrides.
///
/// A project promotes a log-only kind to deny (or demotes a denying one) by adding
/// `"deviation_severity": {"<kind>": "deny"}` to its graph.json policy. An unknown
/// kind or an unparseable value falls back to the registry default rather than
/// guessing, so a typo weakens nothing.
pub fn effective_severity(kind: &str) -> Severity {
    let policy = crate::orchestrator::fsm::graph().policy;
    effective_severity_with(kind, &policy.deviation_severity)
}

pub fn effective_severity_with(
    kind: &str,
    overrides: &std::collections::BTreeMap<String, String>,
) -> Severity {
    let base = default_severity(kind).unwrap_or(Severity::Log);
    match overrides.get(kind).and_then(|v| Severity::parse(v)) {
        Some(sev) => sev,
        None => base,
    }
}

/// Override keys naming a kind this build has never heard of.
///
/// Same non-fatal-warning treatment as `Graph::unknown_policy_keys`, and for the
/// same reason: a policy authored against a newer binary must not drop an older one
/// back to compiled defaults, but a typo that silently configures nothing is exactly
/// the failure the registry exists to make visible.
pub fn unknown_severity_overrides(
    overrides: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    overrides
        .keys()
        .filter(|k| !kind_is_known(k))
        .cloned()
        .collect()
}

/// Override values that are neither "deny" nor "log".
pub fn invalid_severity_values(
    overrides: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    overrides
        .iter()
        .filter(|(_, v)| Severity::parse(v).is_none())
        .map(|(k, v)| format!("{k}={v}"))
        .collect()
}
