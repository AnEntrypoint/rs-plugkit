#[cfg(not(target_arch = "wasm32"))]
pub use rs_exec::background_tasks;
#[cfg(not(target_arch = "wasm32"))]
pub use rs_exec::daemon;
#[cfg(not(target_arch = "wasm32"))]
pub use rs_exec::rpc_client;
#[cfg(not(target_arch = "wasm32"))]
pub use rs_exec::runner;
#[cfg(not(target_arch = "wasm32"))]
pub use rs_exec::runtime;

pub use rs_codeinsight::{analyze, collect_files, matches_ignore_pattern, AnalyzeOptions, AnalysisOutput};

pub use rs_search::{bm25, context, run_search, scanner};
#[cfg(not(target_arch = "wasm32"))]
pub use rs_search::mcp as search_mcp;

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn plugkit_version() -> *const u8 {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr()
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn plugkit_alloc(len: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(len);
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn plugkit_free(ptr: *mut u8, len: usize) {
    unsafe { let _ = Vec::from_raw_parts(ptr, len, len); }
}
