use super::events::{emit_event, install_panic_hook, log_deviation_push};
use super::host_abi::{
    crawl_cdp_call, git_call, git_call_argv, host_env_get, host_exec_js, host_fetch,
    host_fs_readdir, host_kv_delete, host_kv_get, host_kv_put, host_kv_query, host_now_ms,
    host_read, pack, plugin_call as call_plugin, plugin_call_text, read_str, unpack_to_string,
    unpack_to_value,
};
use crate::commit_scope::{
    blanket_stage_refusal, pathspec_covers_path, unrequested_stage_refusal,
};
use crate::git_index_lock::resolved_note;
use crate::orchestrator::yaml_util::base64_decode;
use serde_json::{json, Value};

mod git;
mod search;
mod memory;
use git::*;
pub(crate) use git::GIT_PROTECTED_PATHSPECS;
use search::*;
pub use memory::*;
use super::{dangling_refs, host_abi};

pub fn plugin_ok(resp: &Value) -> bool {
    resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
}

pub const PLUGIN_FAIL_UNKNOWN_PLUGIN: &str = "unknown_plugin";
pub const PLUGIN_FAIL_NOT_LOADED: &str = "plugin_not_loaded_yet";
pub const PLUGIN_FAIL_DEADLINE: &str = "deadline_exceeded";
pub const PLUGIN_FAIL_MALFORMED: &str = "malformed_response";
pub const PLUGIN_FAIL_HOST_EMPTY: &str = "host_returned_empty";
pub const PLUGIN_FAIL_PLUGIN_ERROR: &str = "plugin_error";
pub const PLUGIN_FAIL_HOST_LOST_RESPONSE_RETRYABLE: &str = "plugin_response_lost";

fn text_names_deadline_exceeded(low: &str) -> bool {
    (low.contains("deadline") && low.contains("exceed"))
        || (low.contains(" exceeded ") && low.contains("executing verb"))
}

fn text_names_unknown_plugin(low: &str) -> bool {
    low.contains("unknown plugin") || low.contains("not registered")
}

fn text_names_response_lost(low: &str) -> bool {
    low.contains("plugin_response_lost") || low.contains("never reached the guest")
}

pub fn plugin_failure_code(resp: &Value) -> &'static str {
    if resp.is_null() {
        return PLUGIN_FAIL_HOST_EMPTY;
    }
    if let Value::String(raw) = resp {
        let low = raw.to_ascii_lowercase();
        if text_names_response_lost(&low) {
            return PLUGIN_FAIL_HOST_LOST_RESPONSE_RETRYABLE;
        }
        if text_names_deadline_exceeded(&low) {
            return PLUGIN_FAIL_DEADLINE;
        }
        if text_names_unknown_plugin(&low) {
            return PLUGIN_FAIL_UNKNOWN_PLUGIN;
        }
        return PLUGIN_FAIL_MALFORMED;
    }
    let raw_err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("");
    let low = raw_err.to_ascii_lowercase();
    if low == PLUGIN_FAIL_UNKNOWN_PLUGIN || text_names_unknown_plugin(&low) {
        return PLUGIN_FAIL_UNKNOWN_PLUGIN;
    }
    if low == PLUGIN_FAIL_NOT_LOADED || low.contains("not loaded") {
        return PLUGIN_FAIL_NOT_LOADED;
    }
    if text_names_response_lost(&low) {
        return PLUGIN_FAIL_HOST_LOST_RESPONSE_RETRYABLE;
    }
    if text_names_deadline_exceeded(&low) {
        return PLUGIN_FAIL_DEADLINE;
    }
    PLUGIN_FAIL_PLUGIN_ERROR
}

fn plugin_error_message_or_bare_string_fallback(
    resp: &Value,
    message_when_shape_unrecognized: &str,
) -> String {
    match resp {
        Value::String(raw) => raw.clone(),
        Value::Null => "host_plugin_call returned no bytes".to_string(),
        _ => resp
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or(message_when_shape_unrecognized)
            .to_string(),
    }
}

pub fn plugin_error_detail(resp: &Value, message_when_shape_unrecognized: &str) -> Value {
    let code = plugin_failure_code(resp);
    let message =
        plugin_error_message_or_bare_string_fallback(resp, message_when_shape_unrecognized);
    let mut detail = json!({
        "error": message,
        "plugin_failure": code,
        "retryable": code == PLUGIN_FAIL_NOT_LOADED
            || code == PLUGIN_FAIL_DEADLINE
            || code == PLUGIN_FAIL_HOST_LOST_RESPONSE_RETRYABLE,
    });
    if let Some(p) = resp.get("plugin").and_then(|v| v.as_str()) {
        detail["plugin"] = json!(p);
    }
    detail
}

fn plugin_error(resp: &Value, message_when_shape_unrecognized: &str) -> String {
    let code = plugin_failure_code(resp);
    let message =
        plugin_error_message_or_bare_string_fallback(resp, message_when_shape_unrecognized);
    format!("[{}] {}", code, message)
}

fn err_plugin(verb: &str, resp: &Value, fallback: &str) -> u64 {
    err_json(verb, plugin_error_detail(resp, fallback))
}

pub fn vec_search_local(embedding: &Value, namespace: &str, k: u32) -> Value {
    let (hits, _) = rssearch_vector_hits(embedding, namespace, k, false);
    hits
}

pub fn embed_query(query: &str) -> Value {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        emit_event(
            "embed_query_failed",
            json!({ "reason": "empty query after trim" }),
        );
        return Value::Null;
    }
    let resp = call_plugin("bert", "embed", &json!({ "text": query, "kind": "query" }));
    if !plugin_ok(&resp) {
        emit_event(
            "embed_query_failed",
            json!({
                "reason": "bert plugin call failed",
                "error": plugin_error(&resp, "no error field on bert response"),
                "plugin_failure": plugin_failure_code(&resp),
                "query_len": query.len(),
            }),
        );
        return Value::Null;
    }
    match resp.get("embedding") {
        Some(v) if !v.is_null() => v.clone(),
        _ => {
            emit_event(
                "embed_query_failed",
                json!({
                    "reason": "bert responded ok but carried no embedding field",
                    "query_len": query.len(),
                }),
            );
            Value::Null
        }
    }
}

fn embed_passage(text: &str) -> Option<Value> {
    let resp = call_plugin("bert", "embed", &json!({ "text": text, "kind": "passage" }));
    if !plugin_ok(&resp) {
        emit_event(
            "embed_passage_failed",
            json!({
                "reason": "bert plugin call failed",
                "error": plugin_error(&resp, "no error field on bert response"),
                "plugin_failure": plugin_failure_code(&resp),
                "text_len": text.len(),
            }),
        );
        return None;
    }
    match resp.get("embedding") {
        Some(v) if !v.is_null() => Some(v.clone()),
        _ => {
            emit_event(
                "embed_passage_failed",
                json!({
                    "reason": "bert responded ok but carried no embedding field",
                    "plugin_failure": PLUGIN_FAIL_MALFORMED,
                    "text_len": text.len(),
                }),
            );
            None
        }
    }
}

pub const ERR_CODE_FAILED: &str = "failed";
pub const ERR_CODE_RETIRED_VERB: &str = "retired_verb";
pub const ERR_CODE_UNSUPPORTED: &str = "unsupported_by_design";
pub const ERR_CODE_UNKNOWN_VERB: &str = "unknown_verb";
pub const ERR_CODE_INVALID_ARGS: &str = "invalid_args";
pub const ERR_CODE_PANIC: &str = "panic";
pub const ERR_CODE_GATE_DENIED: &str = "gate_denied";
pub const ERR_CODE_DANGLING_REFERENCE: &str = "dangling_reference";
pub const ERR_CODE_DANGLING_SCAN_UNREADABLE: &str = "dangling_scan_unreadable";

fn shared_store_contract() -> Value {
    json!({
        "identity": "the resolved file path IS the identity -- libsql routes by path alone and ignores the `db` handle name for any real file (that field is consulted only for :memory:), so two callers naming the same file share one store and two projects with different files cannot collide",
        "isolation": "none beyond the path. There is no per-plugin namespace and no ownership claim; any plugin handed the path reaches the whole store, including tables another plugin created.",
        "locking": "the WASI VFS has no OS file locking and serializes with a <db>.lock DIRECTORY. It is created before a write and removed after, so an unclean exit leaves it behind and every later write returns SQLITE_BUSY forever -- no holder to find, no timeout that expires it, survives reboots. A locked error now names the directory and the remedy.",
        "busy_timeout_ms": 20000,
        "busy_timeout_ceiling_rule": "must stay well under the host dispatch deadline (120s). A wait that outlives the epoch budget TRAPS the instance instead of returning a reportable SQLITE_BUSY, and a trap carries no error text -- contention then looks like a crash.",
        "journal_mode": "WAL where the conversion succeeds. It needs an exclusive lock, so it is attempted once per process per path and memoized only on a verified read-back -- PRAGMA journal_mode=WAL returns OK while silently doing nothing when it cannot take the lock.",
        "aggregate_hazard": "an UNFILTERED aggregate over an F32_BLOB vector table answers 0 even when the table is full. Count with a predicate, or with SUM over a GROUP BY subquery, or against the <table>_vec_shadow companion. A bare COUNT(*) reporting 0 is not evidence of an empty store.",
    })
}

fn persisted_paths_compatibility_surface_report() -> Value {
    let paths = [
        ".gm/turn-state.json",
        ".gm/prd.yml",
        ".gm/mutables.yml",
        ".gm/residual-check-fired",
        ".gm/claim-audit-fired",
        ".gm/last-instruction-ts",
        ".gm/exec-spool/.ci-validated",
        ".gm/exec-spool/.gate-deviation-repeats.json",
    ];
    let live: Vec<Value> = paths
        .iter()
        .map(|p| json!({ "path": p, "exists": crate::pkfs::exists(p) }))
        .collect();
    json!({
        "note": "these paths are a compatibility surface -- a reader outside this crate depends on each staying where it is",
        "paths": live,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Capability {
    ProjectPath,
    ProjectPathOrGrantedRoot,
    EnvAllowlist,
    KvNamespace,
    Unguarded,
}

const VERB_CAPABILITIES: &[(&str, Capability)] = &[
    ("fs_read", Capability::ProjectPath),
    ("fs_write", Capability::ProjectPath),
    ("fs_stat", Capability::ProjectPath),
    ("fs_readdir", Capability::ProjectPath),
    ("scan_deps", Capability::ProjectPathOrGrantedRoot),
    ("env_get", Capability::EnvAllowlist),
    ("kv_put", Capability::KvNamespace),
    ("fetch", Capability::Unguarded),
    ("exec_js", Capability::Unguarded),
];

fn verbs_with_capability(cap: Capability) -> Vec<&'static str> {
    VERB_CAPABILITIES
        .iter()
        .filter(|(_, c)| *c == cap)
        .map(|(v, _)| *v)
        .collect()
}

fn guarded_verb_is_dispatchable(verb: &str) -> bool {
    matches!(
        verb,
        "fs_read"
            | "fs_write"
            | "fs_stat"
            | "fs_readdir"
            | "scan_deps"
            | "env_get"
            | "kv_put"
            | "fetch"
            | "exec_js"
    )
}

fn capability_conformance() -> Value {
    let undispatchable: Vec<&str> = VERB_CAPABILITIES
        .iter()
        .map(|(v, _)| *v)
        .filter(|v| !guarded_verb_is_dispatchable(v))
        .collect();
    json!({
        "declared_verbs": VERB_CAPABILITIES.len(),
        "undispatchable": undispatchable,
        "conformant": undispatchable.is_empty(),
        "note": "A non-empty `undispatchable` means the capability table names a verb dispatch cannot reach -- the published guard surface would be describing protection over nothing.",
    })
}

fn guard_surface_report() -> Value {
    json!({
        "path_within_project": {
            "rejects": ["any .. segment", "absolute paths", "paths containing a drive colon"],
            "applied_to": verbs_with_capability(Capability::ProjectPath),
        },
        "read_outside_root_opt_in": {
            "accepts": ["an absolute path outside the project root with no \"..\" segment"],
            "requires": ["the literal boolean body field \"allowOutsideRoot\": true on that same call",
                         "\"allow_outside_root\" and \"allowAbsolute\" are aliases; a non-boolean value is not an opt-in"],
            "applied_to": READ_ONLY_OUTSIDE_ROOT_VERBS,
            "still_rejected": ["any \"..\" segment, with or without the opt-in",
                               "every write verb: fs_write stays project-only and ignores the flag"],
            "note": "The opt-in widens WHICH root a read may address, never whether a read may climb out of one. It is per call and never inferred from the path shape, so a call without it fails exactly as before.",
        },
        "project_path_or_granted_root": {
            "accepts": ["a relative path within the project", "an absolute directory the host grants (an existing directory holding .git, .gm, package.json, Cargo.toml, go.mod or pyproject.toml)"],
            "applied_to": verbs_with_capability(Capability::ProjectPathOrGrantedRoot),
        },
        "env_get": {
            "applied_to": verbs_with_capability(Capability::EnvAllowlist),
            "allowed_exact": ENV_GET_ALLOWED_EXACT,
            "allowed_prefixes": ENV_GET_ALLOWED_PREFIXES,
        },
        "kv_put": {
            "applied_to": verbs_with_capability(Capability::KvNamespace),
            "allowed_namespaces": kv_put_allowed_namespaces(),
        },
        "conformance": capability_conformance(),
        "unguarded": {
            "verbs": verbs_with_capability(Capability::Unguarded),
            "note": "These reach the network and a Node process with no allowlist of their own. That is deliberate -- they exist to run arbitrary caller-supplied work -- but it means the trust boundary for them is the CALLER, not this layer.",
        },
    })
}

fn plugin_response_envelope_contract() -> Value {
    json!({
        "shape": "flat: {ok: bool, <payload-field>: ...} -- NOT wrapped under `data` the way a gm verb response is",
        "failure": "{ok: false, error: string}; a missing `error` on a failed response is classified by plugin_failure_code",
        "hazards": {
            "unfiltered_aggregate_on_vector_table": "DRIVER-SPECIFIC, measured under node:sqlite and NOT reproducing through the agentplug-libsql wasm plugin (2026-08-02: bare COUNT(*) on rssearch_vectors answers 278 there, matching the predicated form exactly). Under node:sqlite an UNFILTERED aggregate over a libsql F32_BLOB vector table answers 0 even when the table is full. Measured on this repo's store: `SELECT COUNT(*) FROM rssearch_vectors` returns 0, and the subquery form without a WHERE also returns 0 -- but the same aggregate WITH any predicate returns 755, reconciling exactly against 428 live plus 327 tombstoned rows. The missing predicate is the trigger, not the aggregate. The query verbs pass SQL through untouched, so nothing stops a caller reading that 0 as an empty knowledgebase and acting on it (dropping the table, re-indexing, reporting data loss). Always count with a predicate, or count the <table>_vec_shadow companion. PRAGMA integrity_check is separately unreliable on these tables -- it fails with `unknown function: libsql_vector_idx()`, which is not corruption.",
        },
        "payload_field_by_verb": {
            "bert.embed": "embedding (plus `dim`); takes {text, kind} where kind=\"query\" selects the query conditioning, anything else is treated as a passage",
            "bert.embed_batch": "embeddings (array, one per input); takes {texts}",
            "libsql.query": "rows",
            "libsql.query_params": "rows",
            "libsql.serialize": "bytes_b64 (also mirrored as `data` for older callers; `bytes_b64` is the contract field)",
            "libsql.list_dbs": "dbs (always empty: connections are no longer tracked by name)",
            "libsql.open/close/begin/commit/rollback": "ACCEPTED BUT INERT -- every exec/query is its own open-operate-close cycle, so these are no-ops kept only so existing callers do not break. A caller must not assume begin/commit gives it a transaction.",
            "treesitter.parse": "nodes (plus `lang`, null when neither ext nor lang resolves -- an unresolved language is ok:true with an empty nodes array, NOT an error)",
            "treesitter.extract_chunks": "chunks (plus `lang`)",
            "treesitter.lang_for_ext": "lang (null when the extension is unrecognized)",
        },
    })
}

fn effective_config_report() -> Value {
    let resolution = crate::config::resolve();
    let cfg = crate::ragconfig::RagConfig::resolved();
    json!({
        "tier": resolution.tier.as_str(),
        "why": resolution.why,
        "version": resolution.config.version,
        "rejected_tiers": resolution.rejected,
        "embed_dim": cfg.dim(),
        "tables": {
            "rssearch": cfg.rssearch.table,
            "code_chunks": cfg.code_chunks.table,
            "git_commits": cfg.git_commits.table,
            "memory_md_meta": cfg.memory_md_tables.meta,
            "memory_md_files": cfg.memory_md_tables.files,
        },
        "scoring": {
            "half_life_ms": cfg.scoring.half_life_ms,
            "recency_floor": cfg.scoring.recency_floor,
            "cos_floor": cfg.scoring.cos_floor_applied_before_recency_rescue,
            "fusion_rrf_k": cfg.scoring.fusion_rrf_k,
            "fusion_identifier_boost": cfg.scoring.fusion_identifier_boost,
            "bm25_k1": cfg.scoring.bm25_k1_term_frequency_saturation,
            "bm25_b": cfg.scoring.bm25_b_document_length_normalization,
            "dedup_jaccard": cfg.scoring.dedup_jaccard_near_duplicate_threshold,
        },
        "bulk_embed": {
            "flat_json_migration_budget_ms": cfg.bulk_embed.flat_json_migration_budget_ms,
            "git_commit_embed_budget_ms": cfg.bulk_embed.git_commit_embed_budget_ms,
        },
        "guards": guard_surface_report(),
        "persisted_paths": persisted_paths_compatibility_surface_report(),
        "shared_store_contract": shared_store_contract(),
        "pipeline": {
            "ttl_ms": cfg.pipeline.ttl_ms,
            "summarize_threshold": cfg.pipeline.summarize_threshold,
            "summarize_target_chars": cfg.pipeline.summarize_target_chars,
            "summarize_max_summary_chars": cfg.pipeline.summarize_max_summary_chars,
            "summarize_input_char_cap": cfg.pipeline.summarize_input_char_cap,
            "summarize_preserve": cfg.pipeline.summarize_preserve,
        },
        "db_path": {
            "state_root_dir": cfg.db_path.state_root_dir,
            "db_filename": cfg.db_path.db_filename,
            "resolved": crate::code_index::project_db_path(None),
        },
        "embed_cache": {
            "query_cache_capacity": cfg.embed_cache.query_cache_capacity,
            "query_cache_ttl_ms": cfg.embed_cache.query_cache_ttl_ms,
            "plain_cache_max_text_bytes": cfg.embed_cache.plain_cache_max_text_bytes,
        },
        "memory_sync": {
            "embed_budget_ms": cfg.memory_sync.embed_budget_ms,
            "total_budget_ms": cfg.memory_sync.total_budget_ms,
            "rekey_rows_deadline_ms": cfg.memory_sync.rekey_rows_deadline_ms,
            "shadow_abort_threshold": cfg.memory_sync.shadow_abort_threshold,
            "rekey_batch_max": cfg.memory_sync.rekey_batch_max,
            "rename_batch_chunk": cfg.memory_sync.rename_batch_chunk,
        },
        "index": {
            "wall_budget_ms": cfg.index.wall_budget_ms,
            "max_file_bytes": cfg.index.max_file_bytes,
            "split_chunk_above_bytes": cfg.index.split_chunk_above_bytes,
            "max_chunks_per_file_per_pass": cfg.index.max_chunks_embedded_per_file_per_pass_count_bound_only,
            "pessimistic_ms_per_chunk": cfg.index.pessimistic_ms_per_chunk_used_only_to_derive_a_budget_bound,
            "prune_enumeration_file_cap": cfg.index.prune_enumeration_file_cap,
            "digest_max_files": cfg.index.digest_max_files,
            "prune_pass_file_limit_floor": cfg.index.prune_pass_file_limit_floor,
            "prune_pass_file_limit_ceiling": cfg.index.prune_pass_file_limit_ceiling,
        },
        "budget": {
            "default_limit": cfg.budget.default_limit,
            "default_k": cfg.budget.default_k,
            "pool_multiplier": cfg.budget.pool_multiplier,
            "pool_floor": cfg.budget.pool_floor,
        },
    })
}

fn next_dispatch_hint_for(verb: &str) -> Value {
    if verb == "instruction" {
        Value::Null
    } else {
        json!("instruction")
    }
}

fn session_not_implemented_in_guest(verb: &str, canonical: &str) -> u64 {
    let reason = format!(
        "{canonical} needs a browser session registry, and this guest holds none: crawl sessions are owned by the host's crawl_cdp entry point and the lightpanda sibling, and neither exposes a list or close call to the guest"
    );
    pack(
        json!({
            "ok": false,
            "verb": verb,
            "error_code": "not_implemented_in_guest",
            "reason": reason,
            "next_dispatch_hint": next_dispatch_hint_for(verb),
        })
        .to_string(),
    )
}

fn err(verb: &str, reason: &str) -> u64 {
    err_coded(verb, ERR_CODE_FAILED, reason)
}

fn err_coded(verb: &str, code: &str, reason: &str) -> u64 {
    pack(
        json!({
            "ok": false,
            "verb": verb,
            "error": reason,
            "error_code": code,
            "next_dispatch_hint": next_dispatch_hint_for(verb),
        })
        .to_string(),
    )
}

fn err_json(verb: &str, detail: Value) -> u64 {
    let mut obj = json!({
        "ok": false,
        "verb": verb,
        "error_code": ERR_CODE_FAILED,
        "next_dispatch_hint": next_dispatch_hint_for(verb),
    });
    if let Some(map) = detail.as_object() {
        for (k, v) in map {
            obj[k] = v.clone();
        }
    }
    pack(obj.to_string())
}

fn err_retry_same_verb(verb: &str, reason: &str) -> u64 {
    pack(
        json!({
            "ok": false,
            "verb": verb,
            "error": reason,
            "error_code": ERR_CODE_INVALID_ARGS,
            "next_dispatch_hint": verb,
        })
        .to_string(),
    )
}

fn ok(verb: &str, data: Value) -> u64 {
    pack(json!({ "ok": true, "verb": verb, "data": data }).to_string())
}

/// A scan that did not see every file still answers, so it stays `ok` -- a caller who read
/// `ok: true` as "the whole tree was searched" would take a partial answer for a complete one.
/// The bound that fired rides beside `ok` instead of inside `data`, where it used to sit behind
/// a page of counters.
fn ok_partial(verb: &str, data: Value, partial_reason: &str) -> u64 {
    pack(json!({ "ok": true, "verb": verb, "partial": true, "partial_reason": partial_reason, "data": data }).to_string())
}

const READ_OUTSIDE_ROOT_OPT_IN_FIELDS: &[&str] =
    &["allowOutsideRoot", "allow_outside_root", "allowAbsolute"];
const READ_ONLY_OUTSIDE_ROOT_VERBS: &[&str] = &["fs_read", "fs_readdir", "fs_stat"];

fn caller_opted_outside_root(body: &Value) -> bool {
    READ_OUTSIDE_ROOT_OPT_IN_FIELDS
        .iter()
        .any(|field| body.get(*field).and_then(|v| v.as_bool()).unwrap_or(false))
}

fn path_has_parent_traversal(path: &str) -> bool {
    path.replace('\\', "/").split('/').any(|seg| seg == "..")
}

fn outside_root_read_granted(path: &str) -> bool {
    if crate::wasm_dispatch::host_allow_root(path) {
        return true;
    }
    let slashed = path.replace('\\', "/");
    let mut ancestor = slashed.as_str();
    while let Some(i) = ancestor.rfind('/') {
        if i == 0 {
            break;
        }
        ancestor = &ancestor[..i];
        if ancestor.ends_with(':') {
            break;
        }
        if crate::wasm_dispatch::host_allow_root(ancestor) {
            return true;
        }
    }
    false
}

fn outside_root_not_granted_message(path: &str) -> String {
    format!("allowOutsideRoot:true was accepted and the path has no \"..\" segment, but the host sandbox will not serve \"{path}\": outside the project root it reads only paths under the user gm root or under a directory carrying a project marker (.git, .gm, package.json, Cargo.toml, go.mod, pyproject.toml). Point the call at such a directory, or read this path with the host's own file-read tool.")
}

fn path_within_project(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    !normalized.split('/').any(|seg| seg == "..")
        && !normalized.starts_with('/')
        && !normalized.contains(':')
}

fn path_outside_project_message(path: &str, read_verb: bool) -> String {
    let Some(root) = super::host_abi::host_cwd_string() else {
        return if read_verb {
            "path must be relative and within the project; pass allowOutsideRoot:true to read outside the root".to_string()
        } else {
            "path must be relative and within the project".to_string()
        };
    };
    let slashed = |s: &str| s.replace('\\', "/");
    let root_slashed = slashed(&root);
    let root_trimmed = root_slashed.trim_end_matches('/');
    let path_slashed = slashed(path);
    let suggestion = path_slashed
        .strip_prefix(root_trimmed)
        .map(|rest| rest.trim_start_matches('/'))
        .filter(|rest| !rest.is_empty() && path_within_project(rest))
        .map(|rest| format!("; the relative form of that path is \"{rest}\""))
        .unwrap_or_default();
    let opt_in_hint = if read_verb {
        "; pass allowOutsideRoot:true to read outside the root (read verbs only -- fs_write is never widened)"
    } else {
        ""
    };
    format!("path must be relative and within the project; the project root is {root}, so pass a path relative to it (for example \"src/main.rs\", not an absolute path or one containing \"..\"){suggestion}{opt_in_hint}")
}

fn path_traversal_message(path: &str) -> String {
    format!("path must not contain a \"..\" segment, even with allowOutsideRoot:true -- the opt-in widens which root a read may address, never whether it may climb out of one; pass the final absolute path directly instead of \"{path}\"")
}

fn project_path_rejection(verb: &str, path: &str, allow_outside_root: bool) -> Option<u64> {
    if path_within_project(path) {
        return None;
    }
    let read_verb = READ_ONLY_OUTSIDE_ROOT_VERBS.contains(&verb);
    if !read_verb || !allow_outside_root {
        return Some(err(verb, &path_outside_project_message(path, read_verb)));
    }
    if path_has_parent_traversal(path) {
        return Some(err(verb, &path_traversal_message(path)));
    }
    if outside_root_read_granted(path) {
        return None;
    }
    Some(err(verb, &outside_root_not_granted_message(path)))
}

fn read_path_rejection(verb: &str, path: &str, body: &Value) -> Option<u64> {
    project_path_rejection(verb, path, caller_opted_outside_root(body))
}

fn paged_lines(content: &str, offset: usize, limit: usize) -> (String, usize, usize, usize) {
    let lines: Vec<&str> = content.split('\n').collect();
    let total = lines.len();
    let start = offset.min(total);
    let end = match limit {
        0 => total,
        n => (start + n).min(total),
    };
    let selected = &lines[start..end];
    let mut out = String::new();
    for (i, line) in selected.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line);
    }
    (out, total, start, end.saturating_sub(start))
}

fn fs_read(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return err("fs_read", "path required -- pass {\"path\":\"<relative path>\"}; add \"offset\"/\"limit\" to read a line range and \"max_bytes\" to cap one chunk");
    }
    if let Some(rejection) = read_path_rejection("fs_read", path, body) {
        return rejection;
    }
    let offset = match body.get("offset").and_then(|v| v.as_u64()) {
        Some(n) => n as usize,
        None => 0,
    };
    let limit = match body.get("limit").and_then(|v| v.as_u64()) {
        Some(n) => n as usize,
        None => 0,
    };
    let max_bytes = match body.get("max_bytes").and_then(|v| v.as_u64()) {
        Some(n) if n > 0 => Some(n as usize),
        _ => None,
    };
    match host_read(path) {
        Some(content) => {
            if offset == 0 && limit == 0 && max_bytes.is_none() {
                return ok("fs_read", Value::String(content));
            }
            let (mut text, total_lines, from_line, returned_lines) =
                paged_lines(&content, offset, limit);
            let mut truncated_at_bytes = false;
            if let Some(cap) = max_bytes {
                if text.len() > cap {
                    let mut end = cap;
                    while end > 0 && !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text = text[..end].to_string();
                    truncated_at_bytes = true;
                }
            }
            ok(
                "fs_read",
                json!({
                    "path": path,
                    "content": text,
                    "total_lines": total_lines,
                    "offset": from_line,
                    "returned_lines": returned_lines,
                    "has_more_lines": from_line + returned_lines < total_lines,
                    "truncated_at_bytes": truncated_at_bytes,
                }),
            )
        }
        None => err("fs_read", "file read failed"),
    }
}

fn content_value_to_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    let items = value.as_array()?;
    let mut joined = items
        .iter()
        .map(|item| match item {
            Value::String(line) => line.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !joined.is_empty() {
        joined.push('\n');
    }
    Some(joined)
}

fn fs_write(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let content = ["content", "data", "text"]
        .iter()
        .find_map(|key| body.get(*key).and_then(content_value_to_text));
    let allow_empty = body
        .get("allow_empty")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if path.is_empty() {
        return err("fs_write", "path required");
    }
    if !path_within_project(path) {
        let mut message = path_outside_project_message(path, false);
        if caller_opted_outside_root(body) {
            message.push_str(
                " allowOutsideRoot is accepted by read verbs only; writes stay project-only.",
            );
        }
        return err("fs_write", &message);
    }
    let received_keys: Vec<String> = body
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    let Some(content) = content else {
        return err_json(
            "fs_write",
            json!({
                "error": format!(
                    "fs_write needs the file contents under one of content|data|text, as a JSON string or as an array of lines -- the body carried {} and none of those keys held a string or an array",
                    if received_keys.is_empty() { "<no keys>".to_string() } else { received_keys.join(", ") }
                ),
                "error_code": ERR_CODE_INVALID_ARGS,
                "accepted_content_keys": ["content", "data", "text"],
                "accepted_content_shapes": [
                    "a JSON string, with \\n for each newline",
                    "an array of lines, joined with \\n plus a trailing newline"
                ],
                "received_keys": received_keys,
                "next_dispatch": "fs_write",
            }),
        );
    };
    if content.is_empty() && !allow_empty {
        return err_json(
            "fs_write",
            json!({
                "error": "refusing to write empty content -- pass allow_empty: true to truncate the file on purpose",
                "error_code": ERR_CODE_INVALID_ARGS,
                "path": path,
                "next_dispatch": "fs_write",
            }),
        );
    }
    if super::host_abi::host_write(path, &content) {
        ok("fs_write", json!({ "bytes": content.len(), "path": path }))
    } else {
        err("fs_write", "write failed")
    }
}

fn fs_readdir(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    if let Some(rejection) = read_path_rejection("fs_readdir", path, body) {
        return rejection;
    }
    let packed = unsafe { host_fs_readdir(path.as_ptr(), path.len() as u32) };
    let v = unpack_to_value(packed);
    if v.is_null() {
        return err("fs_readdir", "empty");
    }
    ok("fs_readdir", v)
}

fn fs_stat(body: &Value) -> u64 {
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("");
    if path.is_empty() {
        return err("fs_stat", "path required");
    }
    if let Some(rejection) = read_path_rejection("fs_stat", path, body) {
        return rejection;
    }
    match super::host_abi::host_stat(path) {
        Some(v) if !v.is_null() => ok("fs_stat", v),
        _ => err("fs_stat", "not found"),
    }
}

fn path_is_absolute(path: &str) -> bool {
    path.starts_with('/') || path.starts_with('\\') || path.as_bytes().get(1) == Some(&b':')
}

fn scan_deps(body: &Value) -> u64 {
    let root = body
        .get("root")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("projectPath").and_then(|v| v.as_str()))
        .filter(|p| !p.is_empty())
        .unwrap_or(".");
    if path_is_absolute(root) {
        if !crate::wasm_dispatch::host_allow_root(root) {
            return err("scan_deps", &format!("root '{root}' is not a real, existing project directory the host will grant access to"));
        }
    } else if !path_within_project(root) {
        return err("scan_deps", "root must be within the project (relative, no '..') or an absolute project directory the host grants");
    }
    ok("scan_deps", crate::scan_deps::scan_deps(body))
}

pub const FETCH_DEFAULT_TIMEOUT_MS: u64 = 60_000;
pub const FETCH_MAX_TIMEOUT_MS: u64 = 600_000;

fn fetch(body: &Value) -> u64 {
    let url = body.get("url").and_then(|v| v.as_str()).unwrap_or("");
    if url.is_empty() {
        return err("fetch", "url required");
    }
    if let Err(reason) = crate::config_path::validate_fetch_url(url) {
        return err_coded("fetch", ERR_CODE_INVALID_ARGS, &reason);
    }
    let has_explicit_timeout = body.get("timeoutMs").is_some()
        || body.get("opts").and_then(|o| o.get("timeoutMs")).is_some();
    let timeout_ms = if has_explicit_timeout {
        match crate::validation::validate_timeout_ms(body, true) {
            Ok(n) if n > FETCH_MAX_TIMEOUT_MS => {
                return err_json(
                    "fetch",
                    json!({
                        "error": "timeoutMs above ceiling",
                        "max": FETCH_MAX_TIMEOUT_MS,
                        "received": n,
                    }),
                );
            }
            Ok(n) => n,
            Err(detail) => return err_json("fetch", detail),
        }
    } else {
        FETCH_DEFAULT_TIMEOUT_MS
    };
    let mut opts_obj = body.get("opts").cloned().unwrap_or_else(|| json!({}));
    if let Some(map) = opts_obj.as_object_mut() {
        for key in ["method", "headers", "body"] {
            if let Some(value) = body.get(key) {
                map.insert(key.to_string(), value.clone());
            }
        }
        map.insert("timeoutMs".to_string(), json!(timeout_ms));
    } else {
        opts_obj = json!({"timeoutMs": timeout_ms});
    }
    let opts = opts_obj.to_string();
    let packed = unsafe {
        host_fetch(
            url.as_ptr(),
            url.len() as u32,
            opts.as_ptr(),
            opts.len() as u32,
        )
    };
    let v = unpack_to_value(packed);
    if v.is_null() {
        return err("fetch", "host_fetch empty");
    }
    let transport_completed = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
    if transport_completed {
        ok("fetch", v)
    } else {
        err_json("fetch", v)
    }
}

const CRAWL_ENGINE_CDP: &str = "cdp";
const CRAWL_ENGINE_LIGHTPANDA: &str = "lightpanda";
const CRAWL_ERR_UNKNOWN_ENGINE: &str = "unknown_engine";

fn split_crawl_engine(body_s: &str) -> (&str, &str) {
    let (first, rest) = body_s.split_once('\n').unwrap_or((body_s, ""));
    match first.trim().strip_prefix("engine=") {
        Some(name) => (name.trim(), rest),
        None => (CRAWL_ENGINE_CDP, body_s),
    }
}

fn crawl(body_s: &str) -> u64 {
    let (engine, request) = split_crawl_engine(body_s);
    crawl_engine(engine, request)
}

fn chrome(body_s: &str) -> u64 {
    let (_, request) = split_crawl_engine(body_s);
    crawl_engine(CRAWL_ENGINE_CDP, request)
}

fn crawl_engine(engine: &str, request: &str) -> u64 {
    match engine {
        CRAWL_ENGINE_CDP => crawl_reply(crawl_cdp_call(request)),
        CRAWL_ENGINE_LIGHTPANDA => crawl_reply(plugin_call_text("lightpanda", "crawl", request)),
        other => err_coded(
            "crawl",
            CRAWL_ERR_UNKNOWN_ENGINE,
            &format!("unknown crawl engine \"{other}\": the first line must be engine=cdp (headful Chrome over CDP, the default) or engine=lightpanda (headless, explicit only)"),
        ),
    }
}

fn crawl_reply(reply: Value) -> u64 {
    match reply {
        Value::Object(_) => pack(reply.to_string()),
        Value::Null => err_coded("crawl", ERR_CODE_FAILED, "host returned no reply for crawl"),
        other => err_coded(
            "crawl",
            ERR_CODE_FAILED,
            &format!("host crawl reply is not a JSON object: {other}"),
        ),
    }
}

const ENV_GET_ALLOWED_EXACT: &[&str] = &["CLAUDE_PROJECT_DIR", "GITHUB_TOKEN", "GH_TOKEN"];
const ENV_GET_ALLOWED_PREFIXES: &[&str] = &["PLUGKIT_", "GM_"];

fn env_get_allowed(key: &str) -> bool {
    ENV_GET_ALLOWED_EXACT.contains(&key)
        || ENV_GET_ALLOWED_PREFIXES.iter().any(|p| key.starts_with(p))
}

fn env_get(body: &Value) -> u64 {
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    if key.is_empty() {
        return err("env_get", "key required");
    }
    if !env_get_allowed(key) {
        return err("env_get", "key not on env_get allowlist");
    }
    let packed = unsafe { host_env_get(key.as_ptr(), key.len() as u32) };
    match unpack_to_string(packed) {
        Some(s) => ok("env_get", Value::String(s)),
        None => ok("env_get", Value::Null),
    }
}

fn lang(body: &Value) -> u64 {
    let project_dir = body
        .get("projectDir")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let command = body.get("command").and_then(|v| v.as_str()).unwrap_or("");
    let code = body.get("code").and_then(|v| v.as_str()).unwrap_or("");
    if project_dir.is_empty() {
        return err("lang", "projectDir required");
    }
    if command.is_empty() {
        return err("lang", "command required");
    }
    let timeout_ms = body
        .get("timeoutMs")
        .and_then(|v| v.as_u64())
        .unwrap_or(35000);
    let runner_js = format!(
        r#"(async () => {{
  const fs = require('fs');
  const path = require('path');
  const projectDir = {project_dir};
  const command = {command};
  const code = {code};
  const langDir = path.join(projectDir, 'lang');
  if (!fs.existsSync(langDir)) {{ process.stdout.write(JSON.stringify({{ok:false, error:'no-lang-dir', langDir}})); return; }}
  const files = fs.readdirSync(langDir).filter(f => f.endsWith('.js') && f !== 'loader.js');
  const plugins = files.reduce((acc, f) => {{
    try {{
      const p = require(path.join(langDir, f));
      if (p && typeof p.id === 'string' && p.exec && p.exec.match instanceof RegExp && typeof p.exec.run === 'function') acc.push(p);
    }} catch (_) {{}}
    return acc;
  }}, []);
  const plugin = plugins.find(p => p.exec.match.test(command));
  if (!plugin) {{ process.stdout.write(JSON.stringify({{ok:false, error:'no-plugin-matched', command, available: plugins.map(p => p.id)}})); return; }}
  const t0 = Date.now();
  let timer = null;
  try {{
    const out = await Promise.race([
      Promise.resolve(plugin.exec.run(code, projectDir)),
      new Promise((_, rej) => {{ timer = setTimeout(() => rej(new Error('plugin-timeout')), {inner_timeout}); }})
    ]);
    process.stdout.write(JSON.stringify({{ok:true, plugin_id: plugin.id, output: String(out), ms: Date.now() - t0}}));
  }} catch (e) {{
    process.stdout.write(JSON.stringify({{ok:false, error: String(e && e.message || e), plugin_id: plugin.id, ms: Date.now() - t0}}));
  }} finally {{
    if (timer) clearTimeout(timer);
  }}
}})().catch(e => {{ process.stdout.write(JSON.stringify({{ok:false, error: String(e && e.message || e)}})); }})"#,
        project_dir = serde_json::to_string(project_dir).unwrap_or_else(|_| "\"\"".to_string()),
        command = serde_json::to_string(command).unwrap_or_else(|_| "\"\"".to_string()),
        code = serde_json::to_string(code).unwrap_or_else(|_| "\"\"".to_string()),
        inner_timeout = timeout_ms.saturating_sub(2000).max(1000),
    );
    let opts = json!({"timeoutMs": timeout_ms}).to_string();
    let packed = unsafe {
        host_exec_js(
            runner_js.as_ptr(),
            runner_js.len() as u32,
            opts.as_ptr(),
            opts.len() as u32,
        )
    };
    match unpack_to_string(packed) {
        Some(s) => {
            let envelope: Value = serde_json::from_str(&s).unwrap_or(Value::Null);
            if envelope.is_null() {
                return err("lang", "host_exec_js returned non-JSON");
            }
            let stdout = envelope
                .get("stdout")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let exit_code = envelope
                .get("exit_code")
                .and_then(|v| v.as_i64())
                .unwrap_or(-1);
            let timed_out = envelope
                .get("timed_out")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if timed_out {
                return err("lang", "host_exec_js timed out");
            }
            if exit_code != 0 {
                let stderr = envelope
                    .get("stderr")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                return err_json(
                    "lang",
                    json!({"error":"runner exit non-zero","exit_code":exit_code,"stderr":stderr,"stdout":stdout}),
                );
            }
            let inner: Value =
                serde_json::from_str(stdout).unwrap_or_else(|_| Value::String(stdout.to_string()));
            ok("lang", inner)
        }
        None => err("lang", "host_exec_js returned empty"),
    }
}

const EXEC_JS_SUPPORTED_BODY_SHAPES: &str = "[timeoutMs=<ms>\\n]<code> (timeoutMs is the enforced wall-clock limit: default 300000, hard ceiling 900000, the process tree is killed at expiry)";

fn exec_js(body: &Value, body_s: &str) -> u64 {
    if body.is_object() {
        return err_json(
            "exec_js",
            json!({
                "error": "exec_js takes a plain-text body, never a JSON object. Send the raw code itself as the dispatch body, with an optional leading timeoutMs=<ms> line.",
                "error_code": ERR_CODE_INVALID_ARGS,
                "supported_shapes": EXEC_JS_SUPPORTED_BODY_SHAPES,
                "received_keys": body.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
            }),
        );
    }
    let (prefix_timeout_ms, code) = strip_timeout_ms_prefix_directive(body_s);
    if code.trim().is_empty() {
        return err_json(
            "exec_js",
            json!({
                "error": "exec_js body is empty -- provide raw code as the dispatch body",
                "error_code": ERR_CODE_INVALID_ARGS,
                "supported_shapes": EXEC_JS_SUPPORTED_BODY_SHAPES,
            }),
        );
    }
    let opts = match prefix_timeout_ms {
        Some(n) if n >= crate::validation::MIN_TIMEOUT_MS => json!({"timeoutMs": n}),
        Some(n) => {
            return err_json(
                "exec_js",
                json!({
                    "error": "timeoutMs below floor",
                    "error_code": ERR_CODE_INVALID_ARGS,
                    "min": crate::validation::MIN_TIMEOUT_MS,
                    "received": n,
                }),
            )
        }
        None => json!({}),
    }
    .to_string();
    let packed = unsafe {
        host_exec_js(
            code.as_ptr(),
            code.len() as u32,
            opts.as_ptr(),
            opts.len() as u32,
        )
    };
    match unpack_to_string(packed) {
        Some(s) => ok("exec_js", Value::String(s)),
        None => ok("exec_js", Value::Null),
    }
}

fn kv_get(body: &Value) -> u64 {
    let ns = body
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    if key.is_empty() {
        return err("kv_get", "key required");
    }
    if let Some(violation) = confinement_violation(body, ns) {
        return err("kv_get", &violation);
    }
    if let Some(violation) = capability_access_violation(body, ns) {
        return violation;
    }
    let packed =
        unsafe { host_kv_get(ns.as_ptr(), ns.len() as u32, key.as_ptr(), key.len() as u32) };
    match unpack_to_string(packed) {
        Some(s) => ok("kv_get", Value::String(s)),
        None => ok("kv_get", Value::Null),
    }
}

fn capability_access_violation(body: &Value, namespace: &str) -> Option<u64> {
    let accessor = body.get("discipline").and_then(|v| v.as_str())?;
    if accessor == namespace {
        return None;
    }
    match crate::orchestrator::capability_proxy::resolve(accessor, namespace) {
        Ok(_) => None,
        Err(e) => Some(err_json(
            "kv_access",
            json!({
                "error": e.message(),
                "error_code": e.code(),
                "accessor": accessor,
                "capability": namespace,
            }),
        )),
    }
}

fn confinement_violation(body: &Value, namespace: &str) -> Option<String> {
    let claimed = body.get("discipline").and_then(|v| v.as_str())?;
    if claimed == namespace {
        return None;
    }
    let enabled = crate::orchestrator::discipline_note::enabled_names();
    if enabled.iter().any(|n| n == namespace) {
        Some(format!(
            "confinement violation (Cordis Definition 48): component '{}' may not write namespace '{}' belonging to another enabled component",
            claimed, namespace
        ))
    } else {
        None
    }
}

const KV_PUT_ALLOWED_NAMESPACES: &[&str] = &["default", "session", "config", "cache", "user"];

fn kv_put_allowed_namespaces() -> Vec<String> {
    let mut all: Vec<String> = KV_PUT_ALLOWED_NAMESPACES
        .iter()
        .map(|s| s.to_string())
        .collect();
    for extra in &crate::ragconfig::RagConfig::resolved()
        .namespaces
        .kv_put_extra
    {
        if !all.iter().any(|a| a == extra) {
            all.push(extra.clone());
        }
    }
    all
}

fn kv_put_namespace_permitted(ns: &str) -> bool {
    kv_put_allowed_namespaces().iter().any(|a| a == ns)
}

fn kv_put(body: &Value) -> u64 {
    let ns = body
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let val = body.get("value").and_then(|v| v.as_str()).unwrap_or("");
    if key.is_empty() {
        return err("kv_put", "key required");
    }
    if let Some(violation) = confinement_violation(body, ns) {
        return err("kv_put", &violation);
    }
    if let Some(violation) = capability_access_violation(body, ns) {
        return violation;
    }
    if !kv_put_namespace_permitted(ns) {
        return err(
            "kv_put",
            &format!(
                "namespace not permitted; allowed: {}",
                kv_put_allowed_namespaces().join(", ")
            ),
        );
    }
    let rc = unsafe {
        host_kv_put(
            ns.as_ptr(),
            ns.len() as u32,
            key.as_ptr(),
            key.len() as u32,
            val.as_ptr(),
            val.len() as u32,
        )
    };
    if rc != 0 {
        ok("kv_put", json!({"bytes": val.len()}))
    } else {
        err("kv_put", "put failed")
    }
}

fn kv_query(body: &Value) -> u64 {
    let ns = body
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    if let Some(violation) = confinement_violation(body, ns) {
        return err("kv_query", &violation);
    }
    if let Some(violation) = capability_access_violation(body, ns) {
        return violation;
    }
    let q = body.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let packed = unsafe { host_kv_query(ns.as_ptr(), ns.len() as u32, q.as_ptr(), q.len() as u32) };
    let v = unpack_to_value(packed);
    ok("kv_query", v)
}

fn discipline_fanout_namespaces_unioned_from_enabled_txt_and_config(base: &str) -> Vec<String> {
    let mut out = vec![base.to_string()];
    let push_unique_nonblank_noncomment = |name: &str, out: &mut Vec<String>| {
        let name = name.trim();
        if !name.is_empty() && !name.starts_with('#') && !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    };
    if let Some(content) = host_read(".gm/disciplines/enabled.txt") {
        for line in content.lines() {
            push_unique_nonblank_noncomment(line, &mut out);
        }
    }
    for name in &crate::ragconfig::RagConfig::resolved()
        .namespaces
        .discipline_fanout
    {
        push_unique_nonblank_noncomment(name, &mut out);
    }
    out
}

const LIFECYCLE_STALE_WARN_MS: u64 = 30 * 60 * 1000;

fn lifecycle_liveness() -> Value {
    let log = match crate::wasm_dispatch::host_read(".gm/exec-spool/.watcher.log") {
        Some(l) => l,
        None => {
            return json!({ "last_event_age_ms": null, "note": "no .watcher.log yet -- expected on a brand-new project with no dispatches fired" })
        }
    };
    let last_evt_line = log.lines().rev().find_map(|l| l.strip_prefix("evt: "));
    let Some(line) = last_evt_line else {
        return json!({ "last_event_age_ms": null, "note": "watcher.log exists but contains no evt: lines -- the lifecycle-event pipe may be dead (see the 2026-07-22 gm-log outage incident this check targets)" });
    };
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return json!({ "last_event_age_ms": null, "note": "last evt: line in watcher.log failed to parse as JSON" });
    };
    let Some(ts) = v.get("ts").and_then(|t| t.as_u64()) else {
        return json!({ "last_event_age_ms": null, "note": "last evt: line has no ts field" });
    };
    let now = unsafe { host_now_ms() };
    let age_ms = now.saturating_sub(ts);
    json!({
        "last_event_age_ms": age_ms,
        "last_event": v.get("event").cloned().unwrap_or(Value::Null),
        "stale": age_ms > LIFECYCLE_STALE_WARN_MS,
    })
}

fn health_probe_recall() -> Value {
    let probe = json!({ "query": "health check probe query", "limit": 1 });
    let packed = recall(&probe);
    let v = unpack_to_value(packed);
    let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
    json!({ "ok": ok, "error": if ok { Value::Null } else { v.get("error").cloned().unwrap_or(Value::Null) } })
}

fn health_probe_codesearch() -> Value {
    let probe = json!({ "query": "health check probe query", "k": 1 });
    let packed = codesearch(&probe);
    let v = unpack_to_value(packed);
    let ok = v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false);
    json!({ "ok": ok, "error": if ok { Value::Null } else { v.get("error").cloned().unwrap_or(Value::Null) } })
}

const HEALTH_PROBE_FIELDS: &[&str] = &["probes", "probe_subsystems"];
const HEALTH_PROBE_SKIPPED_NOTE: &str = "not run: each probe is a real recall and a real codesearch, measured at 40-160s together on a large project, which no dispatch caller's timeout survives. Pass {\"probes\": true} to run them.";

fn health_probe_skipped() -> Value {
    json!({ "ok": Value::Null, "skipped": true, "note": HEALTH_PROBE_SKIPPED_NOTE })
}

fn health(body: &Value) -> u64 {
    let now = unsafe { host_now_ms() };
    let subsystems: Vec<Value> = crate::mediator::all_verbs_by_subsystem()
        .into_iter()
        .map(|(sub, verbs)| json!({ "subsystem": sub.as_str(), "verbs": verbs }))
        .collect();
    let aliases: Vec<Value> = crate::mediator::alias_table()
        .into_iter()
        .map(|(alias, canonical, lang_preserved)| {
            json!({
                "alias": alias,
                "canonical": canonical,
                "lang_preserved": lang_preserved,
            })
        })
        .collect();
    fn project_gm_json_pinned_version() -> Value {
        match crate::wasm_dispatch::host_read("gm.json")
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| {
                v.get("plugkitVersion")
                    .and_then(|p| p.as_str().map(String::from))
            }) {
            Some(tag) => Value::String(tag),
            None => Value::Null,
        }
    }

    let probes_requested = HEALTH_PROBE_FIELDS
        .iter()
        .any(|field| body.get(*field).and_then(|v| v.as_bool()).unwrap_or(false));
    let (recall_health, codesearch_health) = if probes_requested {
        (health_probe_recall(), health_probe_codesearch())
    } else {
        (health_probe_skipped(), health_probe_skipped())
    };
    let subsystems_healthy = match (
        recall_health.get("ok").and_then(|b| b.as_bool()),
        codesearch_health.get("ok").and_then(|b| b.as_bool()),
    ) {
        (Some(recall_ok), Some(codesearch_ok)) => recall_ok && codesearch_ok,
        _ => true,
    };

    ok(
        "health",
        json!({
            "ok": subsystems_healthy,
            "version": env!("CARGO_PKG_VERSION"),
            "crate_version": env!("CARGO_PKG_VERSION"),
            "source_sha": env!("PLUGKIT_SOURCE_SHA"),
            "loaded_module_is_compiled_version_above_not_project_pin": true,
            "project_gm_json_pinned_version": project_gm_json_pinned_version(),
            "now": now,
            "imports": super::host_abi::HOST_IMPORTS,
            "imports_count": super::host_abi::HOST_IMPORTS.len(),
            "effective_config": effective_config_report(),
            "plugin_response_envelope": plugin_response_envelope_contract(),
            "subsystems": subsystems,
            "subsystem_probes": { "recall": recall_health, "codesearch": codesearch_health },
            "subsystem_probes_requested": probes_requested,
            "verb_aliases": aliases,
            "error_codes": [
                ERR_CODE_FAILED, ERR_CODE_RETIRED_VERB, ERR_CODE_UNSUPPORTED,
                ERR_CODE_UNKNOWN_VERB, ERR_CODE_INVALID_ARGS, ERR_CODE_PANIC, ERR_CODE_GATE_DENIED,
                ERR_CODE_DANGLING_REFERENCE,
            ],
            "plugin_failure_codes": [
                PLUGIN_FAIL_UNKNOWN_PLUGIN, PLUGIN_FAIL_NOT_LOADED, PLUGIN_FAIL_DEADLINE,
                PLUGIN_FAIL_MALFORMED, PLUGIN_FAIL_HOST_EMPTY, PLUGIN_FAIL_PLUGIN_ERROR,
                PLUGIN_FAIL_HOST_LOST_RESPONSE_RETRYABLE,
            ],
            "lifecycle_liveness": lifecycle_liveness()
        }),
    )
}

fn status(body: &Value) -> u64 {
    let task_id = body.get("taskId").and_then(|v| v.as_u64()).unwrap_or(0);
    if task_id == 0 {
        return err("status", "taskId required");
    }
    let ns = body
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("outbox");
    let key = format!("{}", task_id);
    let packed =
        unsafe { host_kv_get(ns.as_ptr(), ns.len() as u32, key.as_ptr(), key.len() as u32) };
    match unpack_to_string(packed) {
        Some(s) => ok(
            "status",
            serde_json::from_str(&s).unwrap_or(Value::String(s)),
        ),
        None => err("status", "task not found"),
    }
}

fn close(body: &Value) -> u64 {
    let task_id = body.get("taskId").and_then(|v| v.as_u64()).unwrap_or(0);
    if task_id == 0 {
        return err("close", "taskId required");
    }
    let key = format!("{}", task_id);
    let rc = unsafe {
        host_kv_put(
            "outbox".as_ptr(),
            6,
            key.as_ptr(),
            key.len() as u32,
            "closed".as_ptr(),
            6,
        )
    };
    if rc != 0 {
        ok("close", json!({ "taskId": task_id }))
    } else {
        err("close", "close failed")
    }
}

fn forget(body: &Value) -> u64 {
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let ns = body
        .get("namespace")
        .and_then(|v| v.as_str())
        .unwrap_or("default");
    if key.is_empty() {
        return err("forget", "key required");
    }
    let rc =
        unsafe { host_kv_delete(ns.as_ptr(), ns.len() as u32, key.as_ptr(), key.len() as u32) };
    if rc == 0 {
        ok("forget", json!({ "namespace": ns, "key": key }))
    } else {
        err("forget", "delete failed")
    }
}

const ROUTER_MODELS: &[&str] = &["claude-haiku-4-5", "claude-sonnet-4-6", "claude-opus-4-7"];

const ROUTE_BUCKET_CAPS: &[u64] = &[1000, 4000, 16000, 64000];

fn bucket_for_tokens(n: u64) -> u8 {
    for (i, &cap) in ROUTE_BUCKET_CAPS.iter().enumerate() {
        if n <= cap {
            return i as u8;
        }
    }
    4
}

pub fn route_hint(prompt: &str, estimated_tokens: u64) -> Value {
    if prompt.trim().is_empty() {
        return Value::Null;
    }
    serde_json::json!({
        "model": ROUTER_MODELS[0],
        "context_bucket": bucket_for_tokens(estimated_tokens),
        "temperature": 0.7f32,
        "top_p": 0.9f32,
        "confidence": 0.5f32,
        "algo": "rule",
        "exploration": false,
    })
}

fn discipline(body: &Value) -> u64 {
    let action = body
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("list");
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    match action {
        "list" => {
            let packed = unsafe { host_kv_query("disciplines".as_ptr(), 11, "".as_ptr(), 0) };
            ok("discipline", unpack_to_value(packed))
        }
        "get" => {
            if name.is_empty() {
                return err("discipline", "name required for get");
            }
            let packed = unsafe {
                host_kv_get("disciplines".as_ptr(), 11, name.as_ptr(), name.len() as u32)
            };
            match unpack_to_string(packed) {
                Some(s) => ok(
                    "discipline",
                    serde_json::from_str(&s).unwrap_or(Value::String(s)),
                ),
                None => err("discipline", "not found"),
            }
        }
        _ => err("discipline", "unknown action"),
    }
}

const SHELL_DEFAULT_TIMEOUT_MS: u64 = 120_000;
const SHELL_SUPPORTED_BODY_SHAPES: &str =
    "timeoutMs=<ms>\\n<command>, or bare command text (uses the default 120000 ms timeout)";

fn shell_exec(body: &Value, body_s: &str, lang: &str) -> u64 {
    if body.is_object() {
        return err_json(
            lang,
            json!({
                "error": format!("{lang} takes a plain-text body, never a JSON object. Send the raw command/script itself as the dispatch body, with an optional leading timeoutMs=<ms> line."),
                "error_code": ERR_CODE_INVALID_ARGS,
                "supported_shapes": SHELL_SUPPORTED_BODY_SHAPES,
                "received_keys": body.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()).unwrap_or_default(),
            }),
        );
    }
    let (prefix_timeout_ms, code) = strip_timeout_ms_prefix_directive(body_s);
    if code.trim().is_empty() {
        return err_json(
            lang,
            json!({
                "error": format!("{lang} body is empty -- provide a raw command/script as the dispatch body"),
                "error_code": ERR_CODE_INVALID_ARGS,
            }),
        );
    }
    let timeout_ms = match prefix_timeout_ms {
        Some(n) if n >= crate::validation::MIN_TIMEOUT_MS => n,
        Some(n) => {
            return err_json(
                lang,
                json!({
                    "error": "timeoutMs below floor",
                    "error_code": ERR_CODE_INVALID_ARGS,
                    "min": crate::validation::MIN_TIMEOUT_MS,
                    "received": n,
                }),
            )
        }
        None => SHELL_DEFAULT_TIMEOUT_MS,
    };
    let opts = json!({ "lang": lang, "timeoutMs": timeout_ms }).to_string();
    let packed = unsafe {
        host_exec_js(
            code.as_ptr(),
            code.len() as u32,
            opts.as_ptr(),
            opts.len() as u32,
        )
    };
    match unpack_to_string(packed) {
        Some(s) => ok(lang, Value::String(s)),
        None => ok(
            lang,
            json!({ "note": "emulated via thebird host_exec_js", "lang": lang }),
        ),
    }
}

fn db_display_label_not_identity(body: &Value) -> String {
    body.get("db_name")
        .or_else(|| body.get("db"))
        .and_then(|v| v.as_str())
        .unwrap_or("main")
        .to_string()
}

fn db_identity_path(body: &Value) -> String {
    match body.get("path").and_then(|v| v.as_str()) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => crate::code_index::project_db_path(None),
    }
}

fn sql_open(body: &Value) -> u64 {
    let path = db_identity_path(body);
    let name = db_display_label_not_identity(body);
    let resp = call_plugin("libsql", "open", &json!({ "db": name, "path": path }));
    if plugin_ok(&resp) {
        ok("sql_open", json!({ "path": path, "db_name": name }))
    } else {
        err_plugin("sql_open", &resp, "open failed")
    }
}

fn sql_close(body: &Value) -> u64 {
    let name = db_display_label_not_identity(body);
    let path = db_identity_path(body);
    let resp = call_plugin("libsql", "close", &json!({ "db": name, "path": path }));
    if plugin_ok(&resp) {
        ok("sql_close", json!({ "db_name": name }))
    } else {
        err_plugin("sql_close", &resp, "close failed")
    }
}

fn sql_list_dbs(_body: &Value) -> u64 {
    let resp = call_plugin("libsql", "list_dbs", &json!({}));
    let names = resp.get("dbs").cloned().unwrap_or_else(|| json!([]));
    ok("sql_list_dbs", json!({ "dbs": names }))
}

fn sql_exec(body: &Value) -> u64 {
    let sql = match body.get("sql").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return err("sql_exec", "missing sql"),
    };
    let name = db_display_label_not_identity(body);
    let path = db_identity_path(body);
    let resp = call_plugin(
        "libsql",
        "exec",
        &json!({ "db": name, "path": path, "sql": sql }),
    );
    if plugin_ok(&resp) {
        ok("sql_exec", json!({}))
    } else {
        err_plugin("sql_exec", &resp, "exec failed")
    }
}

fn sql_query(body: &Value) -> u64 {
    let sql = match body.get("sql").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return err("sql_query", "missing sql"),
    };
    let name = db_display_label_not_identity(body);
    let path = db_identity_path(body);
    let resp = call_plugin(
        "libsql",
        "query",
        &json!({ "db": name, "path": path, "sql": sql }),
    );
    if plugin_ok(&resp) {
        let rows = resp.get("rows").cloned().unwrap_or_else(|| json!([]));
        ok("sql_query", json!({ "rows": rows }))
    } else {
        err_plugin("sql_query", &resp, "query failed")
    }
}

fn apply_cache_budget_overrides_from(cfg: &mut crate::cache::CacheConfig, source: &Value) {
    if let Some(n) = source
        .get("max_entries_per_namespace")
        .and_then(|v| v.as_u64())
    {
        cfg.max_entries_per_namespace = n as usize;
    }
    if let Some(n) = source
        .get("max_bytes_per_namespace")
        .and_then(|v| v.as_u64())
    {
        cfg.max_bytes_per_namespace = n as usize;
    }
    if let Some(n) = source.get("max_value_bytes").and_then(|v| v.as_u64()) {
        cfg.max_value_bytes = n as usize;
    }
    if let Some(n) = source.get("default_ttl_ms").and_then(|v| v.as_i64()) {
        cfg.default_ttl_ms = Some(n);
    }
}

fn cache_cfg_from_defaults_then_vendored_config_then_this_call_body(
    body: &Value,
) -> crate::cache::CacheConfig {
    let mut cfg = crate::cache::DEFAULTS;
    let resolved = crate::config::resolve().config.value;
    if let Some(vendored_cache_section) = resolved.get("cache") {
        apply_cache_budget_overrides_from(&mut cfg, vendored_cache_section);
    }
    apply_cache_budget_overrides_from(&mut cfg, body);
    cfg
}

fn cache_err_with_machine_readable_kind(verb: &str, e: crate::cache::CacheError) -> u64 {
    err_json(
        verb,
        json!({ "error": e.message(), "error_kind": e.kind() }),
    )
}

fn cache_get(body: &Value) -> u64 {
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let cfg = cache_cfg_from_defaults_then_vendored_config_then_this_call_body(body);
    match crate::cache::get(&cfg, ns, key) {
        Ok(Some(entry)) => ok(
            "cache_get",
            json!({ "hit": true, "entry": entry.to_json() }),
        ),
        Ok(None) => ok(
            "cache_get",
            json!({ "hit": false, "namespace": ns, "key": key }),
        ),
        Err(e) => cache_err_with_machine_readable_kind("cache_get", e),
    }
}

fn cache_put(body: &Value) -> u64 {
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let value = match body.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(v) if !v.is_null() => v.to_string(),
        _ => return err("cache_put", "value required"),
    };
    let ttl = body.get("ttl_ms").and_then(|v| v.as_i64());
    let cfg = cache_cfg_from_defaults_then_vendored_config_then_this_call_body(body);
    match crate::cache::put(&cfg, ns, key, &value, ttl) {
        Ok(hash) => ok(
            "cache_put",
            json!({ "content_hash": hash, "bytes": value.len() }),
        ),
        Err(e) => cache_err_with_machine_readable_kind("cache_put", e),
    }
}

fn cache_invalidate(body: &Value) -> u64 {
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
    let cfg = cache_cfg_from_defaults_then_vendored_config_then_this_call_body(body);
    match body
        .get("key")
        .and_then(|v| v.as_str())
        .filter(|k| !k.is_empty())
    {
        Some(key) => match crate::cache::invalidate(&cfg, ns, key) {
            Ok(existed) => ok("cache_invalidate", json!({ "removed": existed })),
            Err(e) => cache_err_with_machine_readable_kind("cache_invalidate", e),
        },
        None => match crate::cache::invalidate_namespace(&cfg, ns) {
            Ok(n) => ok("cache_invalidate", json!({ "removed": n, "namespace": ns })),
            Err(e) => cache_err_with_machine_readable_kind("cache_invalidate", e),
        },
    }
}

fn config_sync_now_force_immediate_refresh(_body: &Value) -> u64 {
    let root = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
    let r = crate::config::resolve_forced(&root);
    let mut payload = r.to_json();
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("forced".to_string(), json!(true));
    }
    ok("config-sync-now", payload)
}

fn config_resolve_report_winning_tier_and_any_rejected_tier(_body: &Value) -> u64 {
    let r = crate::config::resolve();
    let mut payload = r.to_json();

    if let Some(obj) = payload.as_object_mut() {
        obj.insert(
            "unknown_keys".to_string(),
            json!(r.config.unknown_top_level_keys()),
        );
        match crate::ragconfig::RagConfig::from_value(&r.config.value) {
            Ok(rag) => {
                let mut rag_effective = json!({
                    "embed_dim": rag.embed.dim,
                    "default_namespace": rag.namespaces.default,
                    "recall_default_limit": rag.budget.default_limit,
                    "recall_pool_multiplier": rag.budget.pool_multiplier,
                    "recall_pool_floor": rag.budget.pool_floor,
                    "recall_default_k": rag.budget.default_k,
                    "index_wall_budget_ms": rag.index.wall_budget_ms,
                    "index_max_file_bytes": rag.index.max_file_bytes,
                    "index_max_chunks_per_file_per_pass": rag.index.max_chunks_embedded_per_file_per_pass_count_bound_only,
                    "index_pessimistic_ms_per_chunk": rag.index.pessimistic_ms_per_chunk_used_only_to_derive_a_budget_bound,
                    "index_extra_skip_dirs": rag.index.extra_skip_dirs_appended_to_builtins_never_replacing,
                    "index_extra_skip_file_suffixes": rag.index.extra_skip_file_suffixes_appended_to_builtins_never_replacing,
                    "index_force_include_path_substrings": rag.index.force_include_path_substrings_overriding_every_skip,
                    "scoring_bm25_k1": rag.scoring.bm25_k1_term_frequency_saturation,
                    "scoring_bm25_b": rag.scoring.bm25_b_document_length_normalization,
                    "scoring_recency_floor": rag.scoring.recency_floor,
                    "scoring_cos_floor": rag.scoring.cos_floor_applied_before_recency_rescue,
                    "scoring_dedup_jaccard_threshold": rag.scoring.dedup_jaccard_near_duplicate_threshold,
                    "scoring_half_life_ms": rag.scoring.half_life_ms,
                    "scoring_fusion_rrf_k": rag.scoring.fusion_rrf_k,
                    "scoring_fusion_identifier_boost": rag.scoring.fusion_identifier_boost,
                    "pipeline_ttl_ms": rag.pipeline.ttl_ms,
                    "pipeline_summarize_threshold": rag.pipeline.summarize_threshold,
                    "pipeline_max_result_bytes": rag.pipeline.max_result_bytes_advertised_and_enforced_by_one_field,
                    "pipeline_max_attempts": rag.pipeline.max_attempts,
                });
                if let Some(rag_obj) = rag_effective.as_object_mut() {
                    rag_obj.insert("rssearch_table".to_string(), json!(rag.rssearch.table));
                    rag_obj.insert("rssearch_index".to_string(), json!(rag.rssearch.index));
                    rag_obj.insert(
                        "git_commits_table".to_string(),
                        json!(rag.git_commits.table),
                    );
                    rag_obj.insert(
                        "git_commits_index".to_string(),
                        json!(rag.git_commits.index),
                    );
                    rag_obj.insert(
                        "code_chunks_table".to_string(),
                        json!(rag.code_chunks.table),
                    );
                    rag_obj.insert(
                        "code_chunks_index".to_string(),
                        json!(rag.code_chunks.index),
                    );
                    rag_obj.insert(
                        "instruction_payload_ready_wave_limit".to_string(),
                        json!(rag.instruction_payload.ready_wave_limit),
                    );
                    rag_obj.insert(
                        "instruction_payload_instruction_recall_hits".to_string(),
                        json!(rag.instruction_payload.instruction_recall_hits),
                    );
                    rag_obj.insert(
                        "instruction_payload_transition_recall_hits".to_string(),
                        json!(rag.instruction_payload.transition_recall_hits),
                    );
                    rag_obj.insert(
                        "instruction_payload_prompt_excerpt_chars".to_string(),
                        json!(rag.instruction_payload.prompt_excerpt_chars),
                    );
                    rag_obj.insert(
                        "instruction_payload_max_marker_age_ms".to_string(),
                        json!(rag.instruction_payload.max_marker_age_ms),
                    );
                    rag_obj.insert(
                        "instruction_payload_orient_noun_limit".to_string(),
                        json!(rag.instruction_payload.orient_noun_limit),
                    );
                    rag_obj.insert(
                        "discipline_note_max_name_len".to_string(),
                        json!(rag.discipline_note.max_name_len_hard_refuse_not_truncate),
                    );
                    rag_obj.insert(
                        "discipline_note_max_text_len".to_string(),
                        json!(rag.discipline_note.max_text_len_hard_refuse_not_truncate),
                    );
                    rag_obj.insert(
                        "discipline_note_active_policies_instruction_limit".to_string(),
                        json!(
                            rag.discipline_note
                                .active_policies_surfaced_in_instruction_payload_limit
                        ),
                    );
                    rag_obj.insert(
                        "claim_audit_shipped_markers".to_string(),
                        json!(
                            rag.claim_audit
                                .shipped_claim_markers_matched_case_insensitive_substring
                        ),
                    );
                    rag_obj.insert(
                        "claim_audit_scan_paths".to_string(),
                        json!(
                            rag.claim_audit
                                .scan_paths_relative_to_project_root_missing_is_skip_not_error
                        ),
                    );
                }
                obj.insert("rag_effective".to_string(), rag_effective);
            }
            Err(reason) => {
                obj.insert("rag_effective".to_string(), Value::Null);
                obj.insert("rag_rejected".to_string(), Value::String(reason));
            }
        }
    }
    ok("config_resolve", payload)
}

fn dataflow_resolve(_body: &Value) -> u64 {
    let (doc, tier, path) = crate::dataflow::document_detailed();
    let pipelines: serde_json::Map<String, Value> = doc
        .pipelines
        .iter()
        .map(|(name, p)| {
            (
                name.clone(),
                json!({
                    "steps": p.steps.iter().map(|s| json!({"id": s.id, "plugin": s.plugin, "verb": s.verb})).collect::<Vec<_>>(),
                    "fuse": p.fuse.iter().map(|f| json!({"id": f.id, "strategy": f.strategy, "sources": f.sources})).collect::<Vec<_>>(),
                    "output": p.output,
                }),
            )
        })
        .collect();
    ok(
        "dataflow_resolve",
        json!({
            "tier": tier.as_str(),
            "path": path,
            "schema_version": doc.schema_version,
            "pipelines": Value::Object(pipelines),
        }),
    )
}

fn cache_stats(body: &Value) -> u64 {
    let ns = body.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
    let cfg = cache_cfg_from_defaults_then_vendored_config_then_this_call_body(body);
    match crate::cache::stats(&cfg, ns) {
        Ok((entries, bytes)) => ok(
            "cache_stats",
            json!({
                "namespace": ns,
                "entries": entries,
                "bytes": bytes,
                "max_entries_per_namespace": cfg.max_entries_per_namespace,
                "max_bytes_per_namespace": cfg.max_bytes_per_namespace,
                "max_value_bytes": cfg.max_value_bytes,
                "default_ttl_ms": cfg.default_ttl_ms,
            }),
        ),
        Err(e) => cache_err_with_machine_readable_kind("cache_stats", e),
    }
}

fn sql_smoke() -> u64 {
    let owned_path = crate::libsql_wasm::absolute_db_path(".sql-smoke.db");
    let path = owned_path.as_str();
    let mut log: Vec<Value> = Vec::new();
    let _ = call_plugin(
        "libsql",
        "exec",
        &json!({ "path": path, "sql": "DROP TABLE IF EXISTS memos" }),
    );
    let open_resp = call_plugin("libsql", "open", &json!({ "path": path }));
    log.push(json!({ "step": "open", "result": if plugin_ok(&open_resp) { Value::Null } else { Value::String(plugin_error(&open_resp, "open failed")) } }));
    let create_resp = call_plugin(
        "libsql",
        "exec",
        &json!({ "path": path, "sql": "CREATE TABLE memos (id INTEGER PRIMARY KEY, text TEXT, emb F32_BLOB(4))" }),
    );
    log.push(json!({ "step": "create_table", "result": if plugin_ok(&create_resp) { Value::Null } else { Value::String(plugin_error(&create_resp, "exec failed")) } }));
    let insert_resp = call_plugin(
        "libsql",
        "exec",
        &json!({ "path": path, "sql": "INSERT INTO memos(text, emb) VALUES ('hello', vector('[0.1,0.2,0.3,0.4]'))" }),
    );
    log.push(json!({ "step": "insert", "result": if plugin_ok(&insert_resp) { Value::Null } else { Value::String(plugin_error(&insert_resp, "exec failed")) } }));
    let index_resp = call_plugin(
        "libsql",
        "exec",
        &json!({ "path": path, "sql": "CREATE INDEX memos_idx ON memos(libsql_vector_idx(emb, 'metric=cosine'))" }),
    );
    log.push(json!({ "step": "create_index", "result": if plugin_ok(&index_resp) { Value::Null } else { Value::String(plugin_error(&index_resp, "exec failed")) } }));
    let query_resp = call_plugin(
        "libsql",
        "query",
        &json!({ "path": path, "sql": "SELECT id, text, vector_distance_cos(emb, vector('[0.1,0.2,0.3,0.4]')) AS d FROM vector_top_k('memos_idx', vector('[0.1,0.2,0.3,0.4]'), 5) JOIN memos ON memos.rowid = id" }),
    );
    log.push(json!({ "step": "vector_top_k", "rows": resp_rows_or_null(&query_resp) }));
    let _ = call_plugin(
        "libsql",
        "exec",
        &json!({ "path": path, "sql": "DROP TABLE IF EXISTS memos" }),
    );
    let _ = call_plugin("libsql", "close", &json!({ "path": path }));
    let version_resp = call_plugin("libsql", "version", &json!({}));
    let libsql_version = version_resp.get("version").cloned().unwrap_or(Value::Null);
    let failures: Vec<Value> = log
        .iter()
        .filter(|s| s.get("result").map(|r| !r.is_null()).unwrap_or(false))
        .cloned()
        .collect();
    let all_ok = failures.is_empty();
    pack(json!({ "ok": all_ok, "smoke": log, "failures": failures, "libsql_version": libsql_version }).to_string())
}

fn resp_rows_or_null(resp: &Value) -> Value {
    if plugin_ok(resp) {
        resp.get("rows").cloned().unwrap_or(Value::Null)
    } else {
        Value::Null
    }
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    base64_decode(s.trim()).ok()
}

fn sql_serialize(body: &Value) -> u64 {
    let name = db_display_label_not_identity(body);
    let path = db_identity_path(body);
    let resp = call_plugin("libsql", "serialize", &json!({ "db": name, "path": path }));
    if !plugin_ok(&resp) {
        return err_plugin("sql_serialize", &resp, "serialize failed");
    }
    let bytes_b64 = match resp.get("bytes_b64").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return err("sql_serialize", "plugin response missing bytes_b64"),
    };
    let size = resp
        .get("size")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| b64_decode(&bytes_b64).map(|b| b.len() as u64).unwrap_or(0));
    ok(
        "sql_serialize",
        json!({ "bytes_b64": bytes_b64, "size": size, "db_name": name }),
    )
}

fn sql_deserialize(body: &Value) -> u64 {
    let s = match body.get("bytes_b64").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return err("sql_deserialize", "missing bytes_b64"),
    };
    let bytes = match b64_decode(s) {
        Some(b) => b,
        None => return err("sql_deserialize", "invalid base64"),
    };
    let size = bytes.len();
    let name = db_display_label_not_identity(body);
    let path = db_identity_path(body);
    let resp = call_plugin(
        "libsql",
        "deserialize",
        &json!({ "db": name, "path": path, "bytes_b64": s }),
    );
    if plugin_ok(&resp) {
        ok(
            "sql_deserialize",
            json!({ "restored": size, "db_name": name }),
        )
    } else {
        err_plugin("sql_deserialize", &resp, "deserialize failed")
    }
}

fn codeinsight_index(body: &Value) -> u64 {
    let resolved_root = body
        .get("root")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("projectPath").and_then(|v| v.as_str()))
        .filter(|p| !p.is_empty())
        .map(crate::pkfs::anchor);
    let root = resolved_root.as_deref();
    if let Some(root) = root {
        let accessible_directory = crate::wasm_dispatch::host_stat_is_directory(root) == Some(true);
        if !accessible_directory && !crate::wasm_dispatch::host_allow_root(root) {
            return err(
                "codeinsight_index",
                &format!("root '{root}' is not an existing project directory the host will grant access to"),
            );
        }
    }
    let max_files = body
        .get("max_files")
        .and_then(|v| v.as_u64())
        .unwrap_or(500) as usize;
    if body
        .get("dead_code")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let limit = body
            .get("dead_code_limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(200) as usize;
        return pack(
            crate::code_index::index_with_dead_code(root.unwrap_or("."), max_files, limit)
                .to_string(),
        );
    }
    let out = match root {
        Some(r) => crate::code_index::index_at(r, max_files, r),
        None => crate::code_index::index(".", max_files),
    };
    pack(out.to_string())
}

fn body_cwd(body: &Value) -> Option<&str> {
    body.get("cwd")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("repo").and_then(|v| v.as_str()))
        .or_else(|| body.get("root").and_then(|v| v.as_str()))
        .or_else(|| body.get("projectPath").and_then(|v| v.as_str()))
}

fn filter(body: &Value, raw: &str) -> u64 {
    let (data, err_msg) = crate::filter::dispatch(body, raw);
    match err_msg {
        Some(e) => err("filter", &e),
        None => ok("filter", data),
    }
}

#[no_mangle]
pub extern "C" fn dispatch_verb(verb_ptr: u32, verb_len: u32, body_ptr: u32, body_len: u32) -> u64 {
    install_panic_hook();
    let dispatched_verb = read_str(verb_ptr as *const u8, verb_len);
    #[cfg(target_arch = "wasm32")]
    let panic_start_ms = unsafe { host_now_ms() };
    let result =
        std::panic::catch_unwind(|| dispatch_verb_inner(verb_ptr, verb_len, body_ptr, body_len));
    match result {
        Ok(packed) => packed,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic during dispatch".to_string());
            let attributed = if dispatched_verb.is_empty() {
                "dispatch_verb"
            } else {
                dispatched_verb.as_str()
            };
            #[cfg(target_arch = "wasm32")]
            {
                let ms = unsafe { host_now_ms() }.saturating_sub(panic_start_ms);
                emit_event(
                    "dispatch.end",
                    serde_json::json!({
                        "verb": attributed,
                        "ms": ms,
                        "panicked": true,
                        "error_code": ERR_CODE_PANIC,
                    }),
                );
            }
            err_json(
                attributed,
                json!({
                    "error": msg,
                    "error_code": ERR_CODE_PANIC,
                    "panicked": true,
                    "boundary": "dispatch_verb catch_unwind",
                }),
            )
        }
    }
}

fn request_fingerprint(verb: &str, body_s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    verb.hash(&mut hasher);
    body_s.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn verb_body_must_be_json(verb: &str) -> bool {
    !matches!(
        verb,
        "exec_js"
            | "nodejs"
            | "javascript"
            | "node"
            | "js"
            | "python"
            | "py"
            | "bash"
            | "sh"
            | "shell"
            | "zsh"
            | "powershell"
            | "ps1"
            | "crawl"
            | "chrome"
    )
}

fn stamp_request_identity(
    mut value: Value,
    fingerprint: &str,
    body_parse_failed: bool,
    dispatch_id: Option<&str>,
) -> String {
    if let Some(obj) = value.as_object_mut() {
        obj.insert("request_fingerprint".to_string(), json!(fingerprint));
        if body_parse_failed {
            obj.insert("body_parse_error".to_string(), json!(true));
        }
        if let Some(id) = dispatch_id {
            obj.insert("dispatch_id".to_string(), json!(id));
        }
    }
    value.to_string()
}

const BODY_PARSE_SNIPPET_LEAD_BYTES: usize = 32;
const BODY_PARSE_SNIPPET_TRAIL_BYTES: usize = 48;

fn body_parse_failure_response(verb: &str, body_s: &str, err: &serde_json::Error) -> u64 {
    let bytes = body_s.as_bytes();
    let offset = err.column().saturating_sub(1).min(bytes.len());
    let lo = offset.saturating_sub(BODY_PARSE_SNIPPET_LEAD_BYTES);
    let hi = offset.saturating_add(BODY_PARSE_SNIPPET_TRAIL_BYTES).min(bytes.len());
    let snippet = if lo <= hi {
        String::from_utf8_lossy(&bytes[lo..hi]).into_owned()
    } else {
        String::new()
    };
    pack(json!({
        "verb": verb,
        "ok": false,
        "error_code": ERR_CODE_FAILED,
        "error": format!(
            "the {} byte(s) of dispatch body are not valid JSON, so verb \"{}\" never ran: {} at byte {} near {:?}. A backslash inside a JSON string must be doubled, so a Windows path reads \"C:\\\\dev\\\\spoint\" in the body text, or use forward slashes.",
            bytes.len(), verb, err, offset, snippet
        ),
        "body_parse_error": true,
        "body_parse_error_detail": err.to_string(),
        "body_parse_error_offset": offset,
        "body_parse_error_snippet": snippet,
        "body_parse_error_bytes": bytes.len(),
        "executed": false,
        "re_dispatch_safe": true,
        "next_dispatch_hint": "instruction",
    }).to_string())
}

fn extract_session_id_from_plain_text_body(body_s: &str) -> Option<String> {
    let trimmed = body_s.trim_start();
    for prefix in ["sessionId=", "session_id="] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let id = rest.split('\n').next().unwrap_or("").trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
            break;
        }
    }
    None
}

fn strip_timeout_ms_prefix_directive(body_s: &str) -> (Option<u64>, &str) {
    let trimmed = body_s.trim_start();
    for prefix in ["timeoutMs=", "timeout_ms="] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let (value_line, remainder) = rest.split_once('\n').unwrap_or((rest, ""));
            if let Ok(n) = value_line.trim().parse::<u64>() {
                return (Some(n), remainder);
            }
            break;
        }
    }
    (None, body_s)
}

fn strip_path_prefix_directive(body_s: &str) -> Option<(&str, &str)> {
    let trimmed = body_s.trim_start();
    for prefix in ["path=", "fs_path="] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            let (value_line, remainder) = rest.split_once('\n').unwrap_or((rest, ""));
            let value = value_line.trim();
            if !value.is_empty() {
                return Some((value, remainder));
            }
            break;
        }
    }
    None
}

fn raw_file_body_as_json(verb: &str, body_s: &str) -> Option<Value> {
    if verb != "fs_write" {
        return None;
    }
    let (path, contents) = strip_path_prefix_directive(body_s)?;
    Some(json!({ "path": path, "content": contents }))
}

fn dispatch_verb_inner(verb_ptr: u32, verb_len: u32, body_ptr: u32, body_len: u32) -> u64 {
    let verb = read_str(verb_ptr as *const u8, verb_len);
    let raw_body_s = read_str(body_ptr as *const u8, body_len);
    let body_is_json = verb_body_must_be_json(&verb);
    let (caller_timeout_ms, body_s) = if body_is_json {
        let (n, rest) = strip_timeout_ms_prefix_directive(&raw_body_s);
        (n, rest.to_string())
    } else {
        (None, raw_body_s.clone())
    };
    let fingerprint = request_fingerprint(&verb, &body_s);
    let mut parse_failure: Option<serde_json::Error> = if body_is_json && !body_s.is_empty() {
        serde_json::from_str::<Value>(&body_s).err()
    } else {
        None
    };
    let mut body: Value = if body_s.is_empty() { Value::Null } else {
        serde_json::from_str(&body_s).unwrap_or(Value::Null)
    };
    if parse_failure.is_some() {
        if let Some(synthesized) = raw_file_body_as_json(&verb, &body_s) {
            body = synthesized;
            parse_failure = None;
        }
    }
    let body_parse_failed = parse_failure.is_some();
    set_caller_budget(caller_timeout_ms);
    let dispatch_session_id = body.get("sessionId").and_then(|v| v.as_str())
        .or_else(|| body.get("session_id").and_then(|v| v.as_str()))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| extract_session_id_from_plain_text_body(&raw_body_s));
    super::events::set_dispatch_session_id(dispatch_session_id.clone());
    if let Some(root_override) = body.get("git_root_override").and_then(|v| v.as_str()) {
        crate::orchestrator::seed_project_root_override(root_override);
    }
    let root_rejection = reject_if_project_root_unresolvable_before_gm_dir_panics(&verb);
    let root_resolved = root_rejection.is_none();
    let result_packed = match root_rejection {
        Some(rejection) => rejection,
        None => match parse_failure.as_ref() {
            Some(err) => body_parse_failure_response(&verb, &body_s, err),
            None => dispatch_gated_verb(&verb, &body, &body_s),
        },
    };
    set_caller_budget(None);
    super::events::set_dispatch_session_id(None);
    let result_value = super::host_abi::unpack_to_value(result_packed);
    #[cfg(target_arch = "wasm32")]
    let recorded: Option<(String, i64, bool)> = {
        let cwd = body.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
        let exit_code = if result_value
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
        {
            0
        } else {
            1
        };
        // A dispatch the strategy only advised against still ran, so it is still evidence: that is
        // what lets a verb record the success that clears the ranking it was advised under.
        if !root_resolved {
            None
        } else {
            let dispatch_id = crate::dispatch_ledger::record(
                cwd,
                &verb,
                &fingerprint,
                exit_code,
                dispatch_session_id.as_deref(),
            );
            let gate_drift = exit_code != 0
                && crate::orchestrator::dream_rsi::failure_is_gate_drift(&result_value);
            Some((dispatch_id, exit_code, gate_drift))
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    let recorded: Option<(String, i64, bool)> = None;
    let reply = stamp_request_identity(
        result_value,
        &fingerprint,
        body_parse_failed,
        recorded.as_ref().map(|(dispatch_id, _, _)| dispatch_id.as_str()),
    );
    #[cfg(target_arch = "wasm32")]
    if let (Some((dispatch_id, exit_code, gate_drift)), Some(session_id)) =
        (recorded, dispatch_session_id.as_deref())
    {
        let lean_node = body
            .get("lean_node")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|node| !node.is_empty());
        crate::orchestrator::dream_rsi::observe_dispatch(
            crate::orchestrator::dream_rsi::ObservedDispatch {
                session_id,
                dispatch_id: &dispatch_id,
                verb: &verb,
                fingerprint: &fingerprint,
                exit_code,
                gate_drift,
                reply: &reply,
                lean_node,
            },
        );
    }
    pack(reply)
}

fn codeinsight_action(verb: &str, action: &str, body: &Value) -> u64 {
    let mut routed = body.clone();
    if let Some(map) = routed.as_object_mut() {
        map.entry("action").or_insert_with(|| json!(action));
    }
    match crate::code_symbols::handle(&routed) {
        Ok(data) => ok(verb, data),
        Err(reason) => err(verb, &reason),
    }
}

fn codeinsight(body: &Value) -> u64 {
    codeinsight_action("codeinsight", "overview", body)
}

fn callers(body: &Value) -> u64 {
    codeinsight_action("callers", "callers", body)
}

fn callees(body: &Value) -> u64 {
    codeinsight_action("callees", "callees", body)
}

fn impact(body: &Value) -> u64 {
    let mut routed = body.clone();
    if let Some(map) = routed.as_object_mut() {
        map.entry("direction").or_insert_with(|| json!("callees"));
    }
    codeinsight_action("impact", "impact", &routed)
}

fn reject_if_project_root_unresolvable_before_gm_dir_panics(verb: &str) -> Option<u64> {
    if crate::orchestrator::project_root_resolvable() {
        return None;
    }
    Some(err_json(
        verb,
        json!({
            "error": crate::orchestrator::project_root_unresolvable_reason(),
            "error_code": "project_root_unresolvable",
        }),
    ))
}

#[cfg(target_arch = "wasm32")]
fn restamp_long_gap_marker_to_dispatch_completion_if_refresh_verb(verb: &str) {
    if crate::orchestrator::fsm::graph()
        .policy
        .longgap_refresh_verbs
        .iter()
        .any(|v| v == verb)
    {
        let now = unsafe { host_now_ms() };
        let _ = crate::wasm_dispatch::host_write(
            &crate::pkfs::anchor(".gm/last-instruction-ts"),
            &now.to_string(),
        );
    }
}

fn attach_dream_rsi_advisory(
    packed: u64,
    verb: &str,
    reason: &str,
    next_dispatch_hint: &str,
) -> u64 {
    let mut value = super::host_abi::unpack_to_value(packed);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "dream_rsi_advisory".to_string(),
            json!({
                "strategy": "replay-recorded-successes-first",
                "verb": verb,
                "next_dispatch_hint": next_dispatch_hint,
                "admitted_anyway": true,
                "reason": reason,
            }),
        );
    }
    pack(value.to_string())
}

/// The Dream-RSI ranking is advice attached to a dispatch that runs, never a refusal: it is
/// consulted before the dispatch so it reads the state that produced it, and attached after so
/// every arm of `dispatch_verb_unranked` carries it.
fn dispatch_gated_verb(verb: &str, body: &Value, body_s: &str) -> u64 {
    let admission = crate::orchestrator::dream_rsi::admit_dispatch(verb);
    let packed = dispatch_verb_unranked(verb, body, body_s);
    match admission {
        crate::orchestrator::dream_rsi::Admission::Allow => packed,
        crate::orchestrator::dream_rsi::Admission::Advisory {
            reason,
            next_dispatch_hint,
        } => attach_dream_rsi_advisory(packed, verb, &reason, next_dispatch_hint),
    }
}

fn dispatch_verb_unranked(verb: &str, body: &Value, body_s: &str) -> u64 {
    #[cfg(target_arch = "wasm32")]
    let dispatch_start_ms = unsafe { host_now_ms() };
    let gate = crate::gates::check_dispatch(verb, body);
    if !gate.allowed {
        return pack(gate.to_denial_json(verb).to_string());
    }
    if crate::orchestrator::is_orchestrator_verb(verb) && !help_requested(body) {
        let (out, err_msg, code) = crate::orchestrator::dispatch(verb, "", body_s);
        #[cfg(target_arch = "wasm32")]
        {
            let ms = unsafe { host_now_ms() }.saturating_sub(dispatch_start_ms);
            emit_event(
                "dispatch.end",
                serde_json::json!({ "verb": verb, "ms": ms }),
            );
            if verb == "instruction" && code == 0 {
                crate::orchestrator::dream_rsi::stamp_reorientation();
            }
            if !crate::gates::dispatch_serves_no_phase_prose(verb, body) {
                restamp_long_gap_marker_to_dispatch_completion_if_refresh_verb(verb);
                crate::gates::restamp_last_dispatch_to_completion(verb);
            }
        }
        if code == 0 {
            let data: Value = serde_json::from_str(&out).unwrap_or(Value::String(out));
            return ok(verb, data);
        }
        return err_json(
            verb,
            json!({ "error": err_msg, "stdout": out, "exitCode": code }),
        );
    }
    let body = body.clone();
    let body_s = body_s.to_string();
    if help_requested(&body) {
        if let Some(doc) = verb_help_doc(verb) {
            return ok(
                verb,
                json!({ "help": true, "verb": verb, "parameters": doc }),
            );
        }
        return err(verb, &format!(
            "no parameter documentation is published for verb \"{verb}\" -- pass the verb's own body without \"help\" to see its error text, which names the fields it accepts"
        ));
    }
    let result = match verb {
        "fs_read" => fs_read(&body),
        "fs_write" => fs_write(&body),
        "fs_readdir" => fs_readdir(&body),
        "fs_stat" => fs_stat(&body),
        "scan_deps" | "scan-deps" => scan_deps(&body),
        "fetch" => fetch(&body),
        "crawl" => crawl(&body_s),
        "chrome" => chrome(&body_s),
        "session-list" | "session_list" => session_not_implemented_in_guest(verb, "session-list"),
        "session-close-all" | "session_close_all" => session_not_implemented_in_guest(verb, "session-close-all"),
        "env_get" => env_get(&body),
        "kv_get" => kv_get(&body),
        "kv_put" => kv_put(&body),
        "kv_query" => kv_query(&body),
        "exec_js" | "nodejs" | "javascript" | "node" | "js" => exec_js(&body, &body_s),
        "lang" => lang(&body),
        "health" => health(&body),
        "config_resolve" => config_resolve_report_winning_tier_and_any_rejected_tier(&body),
        "config-sync-now" => config_sync_now_force_immediate_refresh(&body),
        "dataflow_resolve" => dataflow_resolve(&body),
        "sql_open" => sql_open(&body),
        "sql_close" => sql_close(&body),
        "sql_list_dbs" => sql_list_dbs(&body),
        "sql_exec" => sql_exec(&body),
        "sql_query" => sql_query(&body),
        "sql_smoke" => sql_smoke(),
        "sql_serialize" => sql_serialize(&body),
        "sql_deserialize" => sql_deserialize(&body),
        "cache_get" => cache_get(&body),
        "cache_put" => cache_put(&body),
        "cache_invalidate" => cache_invalidate(&body),
        "cache_stats" => cache_stats(&body),
        "codeinsight_index" => codeinsight_index(&body),
        "codeinsight" => codeinsight(&body),
        "codesearch" | "code_search" | "search" => codesearch(&body),
        "grep" | "rg" => grep(&body),
        "callers" => callers(&body),
        "callees" => callees(&body),
        "impact" => impact(&body),
        "memorize" => memorize_with_raw(&body, &body_s),
        "memorize-prune" | "memorize_prune" => memorize_prune(&body),
        "memorize-vacuum" | "memorize_vacuum" => memorize_vacuum(&body),
        "memorize-retention" | "memorize_retention" => memorize_retention(&body),
        "recall" => recall(&body),
        "tencentdb-compat-probe" => tencentdb_compat_probe(&body),
        "tencentdb-memory-import" => tencentdb_memory_import_gm_native_memories_reembedded_384dim(&body),
        "python" | "py" => shell_exec(&body, &body_s, "python"),
        "bash" | "sh" | "shell" | "zsh" => shell_exec(&body, &body_s, "bash"),
        "powershell" | "ps1" => shell_exec(&body, &body_s, "powershell"),
        "ssh" => shell_exec(&body, &body_s, "ssh"),
        "go" | "rust" | "c" | "cpp" | "java" | "deno" => shell_exec(&body, &body_s, &verb),
        "status" => status(&body),
        "wait" | "sleep" => err_coded(&verb, ERR_CODE_UNSUPPORTED, "verb not supported: wasm has no real timer/async-sleep primitive here; use exec:sleep (bash `sleep N`, JS setTimeout via exec_js, or PowerShell Start-Sleep) for an actual wait"),
        "close" => close(&body),
        "filter" => filter(&body, &body_s),
        "git_status" => git_status(&body),
        "branch_status" => branch_status(&body),
        "git_push" => git_push(&body),
        "git_add" => git_add(&body),
        "git_commit" => git_commit(&body),
        "git_amend" => git_amend(&body),
        "git_finalize" => git_finalize(&body),
        "git_log" => git_log(&body),
        "git_diff" => git_diff(&body),
        "git_show" => git_show(&body),
        "git_fetch" => git_fetch(&body),
        "git_pull" => git_pull(&body),
            "ci-status" | "ci_status" => ci_status(&body),
            "git_branch" => git_branch(&body),
            "git_remote" => git_remote(&body),
            "git_worktree" => git_worktree(&body),
            "git_checkout" => git_checkout(&body),
        "git_merge" => git_merge(&body),
        "git_merge_abort" => git_merge_abort(&body),
        "git_cherry_pick" => git_cherry_pick(&body),
        "git_stash" => git_stash(&body),
        "git_stash_pop" => git_stash_pop(&body),
        "git_stash_drop" => git_stash_drop(&body),
        "git_stash_list" => git_stash_list(&body),
        "git_init" => git_init(&body),
        "git_branch_delete" => git_branch_delete(&body),
        "git_rm" => git_rm(&body),
        "git_revert" => git_revert(&body),
        "git_reset" => git_reset(&body),
        "git_worktree_add" => git_worktree_add(&body),
        "git_worktree_list" => git_worktree_list(&body),
        "git_worktree_remove" => git_worktree_remove(&body),
        "git_worktree_prune" => git_worktree_prune(&body),
        "git_poll" => git_poll(&body),
        "forget" => forget(&body),
        "learn" => err_coded("learn", ERR_CODE_RETIRED_VERB, "verb retired: the rs-learn crate is removed; memory routes through memorize/recall/memorize-prune (md corpus at .gm/memories + gm.db index)"),
        "cdp" => err_coded("cdp", ERR_CODE_RETIRED_VERB, "verb retired: the standalone cdp verb is removed in this build; dispatch verb crawl with a plain-text body whose first line is engine=cdp (headful Chrome) or engine=lightpanda (headless), then the URL. A session still answered by an older runner build serves cdp; a runner newer than that one retires it"),
        "discipline" => discipline(&body),
        "" => err_coded("", ERR_CODE_INVALID_ARGS, "verb required"),
        _ => err_coded(&verb, ERR_CODE_UNKNOWN_VERB, "unknown verb: this runner build does not register it; check the verb spelling, or a runner/plugin version skew (a second older or newer agentplug-runner serving this project) and run the dispatch verb instruction or docs/verbs.md for the registered set"),
    };
    #[cfg(target_arch = "wasm32")]
    {
        let ms = unsafe { host_now_ms() }.saturating_sub(dispatch_start_ms);
        emit_event(
            "dispatch.end",
            serde_json::json!({ "verb": verb, "ms": ms }),
        );
        crate::gates::restamp_last_dispatch_to_completion(verb);
    }
    result
}
