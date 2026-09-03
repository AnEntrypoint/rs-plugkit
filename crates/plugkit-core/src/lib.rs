pub mod hash;

#[cfg(target_arch = "wasm32")]
pub mod wasm_dispatch;

#[cfg(target_arch = "wasm32")]
pub mod libsql_wasm;

#[cfg(target_arch = "wasm32")]
pub mod shared_db;

#[cfg(target_arch = "wasm32")]
pub mod code_index;

/// Supply-chain scan for the "HiddenSpawn"-class obfuscated dropper
/// (confirmed across 17+ separately-compromised repos, 2026-08): detects
/// the durable structural properties an attacker's payload leaves behind
/// (size/line-ratio disproportion, dense \uXXXX-escape runs decoding to an
/// identifier), not any one incident's specific IP/wallet/module names.
#[cfg(target_arch = "wasm32")]
pub mod scan_deps;

#[cfg(target_arch = "wasm32")]
pub mod embed;

#[cfg(target_arch = "wasm32")]
pub mod embed_marker;

#[cfg(target_arch = "wasm32")]
pub mod pipeline;

#[cfg(target_arch = "wasm32")]
pub mod gitignore;

/// Self-healing removal of retired state artifacts (see module docs): a
/// subsystem retired upstream still leaves its db/state behind in every
/// project that ever ran it, so the tooling reaps its own leftovers.
pub mod legacy_reaper;

#[cfg(target_arch = "wasm32")]
pub mod gates;

#[cfg(target_arch = "wasm32")]
pub mod browser_witness;

#[cfg(target_arch = "wasm32")]
pub mod dispatch_ledger;
pub mod evidence_receipt;

#[cfg(target_arch = "wasm32")]
pub mod poll_detect;

/// Config surface for the RAG/vector layer (namespaces, table names, embed
/// dimension, scoring, limits). Declared ahead of its consumers so they can
/// take `&RagConfig` rather than reading scattered consts.
#[cfg(target_arch = "wasm32")]
pub mod ragconfig;

#[cfg(target_arch = "wasm32")]
pub mod vecstore;

#[cfg(target_arch = "wasm32")]
pub mod vecns;

#[cfg(target_arch = "wasm32")]
pub mod rssearch_vectors;

#[cfg(target_arch = "wasm32")]
pub mod git_commit_vectors;

#[cfg(target_arch = "wasm32")]
pub mod memory_md;

#[cfg(target_arch = "wasm32")]
pub mod tencentdb_memory;

#[cfg(target_arch = "wasm32")]
pub mod tencentdb_compat;

#[cfg(target_arch = "wasm32")]
pub mod mediator;

/// The generalized cache abstraction (see module docs): namespaced get/put/
/// invalidate over the shared libsql store, with explicit TTL staleness, a
/// per-namespace budget, and a miss that a caller can always tell apart from a
/// store failure. Other caches in this tree should route through it, and other
/// plugins can reach it over host_plugin_call via the cache_* verbs.
#[cfg(target_arch = "wasm32")]
pub mod cache;

/// Versioned inter-plugin call envelope + error taxonomy (see module docs).
/// The frozen contract essential plugins are cast against; strictly additive
/// over the unmodified `wasm_dispatch::plugin_call`.
#[cfg(target_arch = "wasm32")]
pub mod plugin_abi;

pub mod pkfs;
/// Allowlist validation for every untrusted string the config chain
/// interpolates into a path or hands to `git` (prose keys, source-spec
/// `path` fields, repo URLs). Declared ahead of `prose`/`config`/`config_sync`
/// so each resolves through one shared set of rules.
pub mod config_path;
pub mod prose;
/// 4-tier config resolution (project-vendored -> in-project repo spec ->
/// user-wide repo spec -> builtin defaults). Generalizes prose.rs's 3-tier
/// instruction-override chain; see the module docs for merge semantics.
pub mod config;
/// Git-backed materialization of the repo-sourced config tiers: the
/// `config::RepoFetcher` implementation config.rs declares as a seam. Probes
/// with `ls-remote` and fetches only on a real sha change, debounced and
/// backed off per source; see the module docs for the offline/concurrency
/// contract it owes the shared plugin instance.
#[cfg(target_arch = "wasm32")]
pub mod config_sync;
pub mod orchestrator;
pub mod filter;
pub mod validation;
/// Data-driven plugin-orchestration pipeline schema: the dataflow counterpart
/// to `orchestrator::fsm`'s phase graph. See module docs for the tiered
/// resolution and step/fuse/condition shape.
pub mod dataflow;
/// Executor walking a `dataflow::Pipeline`, dispatching each step's
/// plugin+verb and threading outputs per its input mapping.
#[cfg(target_arch = "wasm32")]
pub mod dataflow_exec;

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn plugkit_version() -> *const u8 {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr()
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn plugkit_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len);
    assert_eq!(
        v.capacity(),
        len,
        "plugkit_alloc: allocator returned capacity {} for a request of {len}; every packed buffer is reclaimed as Vec::from_raw_parts(p, len, len), so a capacity that differs from the request frees a layout the allocator never issued",
        v.capacity()
    );
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn plugkit_free(ptr: *mut u8, len: usize) {
    let _ = Vec::from_raw_parts(ptr, len, len);
}
