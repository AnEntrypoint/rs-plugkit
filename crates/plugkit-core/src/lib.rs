pub mod hash;

#[cfg(target_arch = "wasm32")]
pub mod wasm_dispatch;

#[cfg(target_arch = "wasm32")]
pub mod libsql_wasm;

#[cfg(target_arch = "wasm32")]
pub mod shared_db;

#[cfg(target_arch = "wasm32")]
pub mod code_index;

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

#[cfg(target_arch = "wasm32")]
pub mod cache;

#[cfg(target_arch = "wasm32")]
pub mod plugin_abi;

pub mod pkfs;
pub mod config_path;
pub mod prose;
pub mod config;
#[cfg(target_arch = "wasm32")]
pub mod config_sync;
pub mod orchestrator;
pub mod filter;
pub mod validation;
pub mod dataflow;
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
