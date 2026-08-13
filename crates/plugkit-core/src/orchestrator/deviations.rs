use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Deny,
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
        "self-reconfig-candidate",
        "a stuck-loop-escalation just fired -- this repeated friction is machine-readable evidence a `fsm-propose-override` proposal may resolve, distinct from the prose-only STUCK LOOP text",
        Severity::Log,
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
        "prd-resolve-fabricated-dispatch",
        "`prd-resolve` supplied a witness_dispatch_id that does not exist in this guest's own dispatch ledger -- the referenced dispatch never actually ran",
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

pub fn unknown_severity_overrides(
    overrides: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    overrides
        .keys()
        .filter(|k| !kind_is_known(k))
        .cloned()
        .collect()
}

pub fn invalid_severity_values(
    overrides: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    overrides
        .iter()
        .filter(|(_, v)| Severity::parse(v).is_none())
        .map(|(k, v)| format!("{k}={v}"))
        .collect()
}
