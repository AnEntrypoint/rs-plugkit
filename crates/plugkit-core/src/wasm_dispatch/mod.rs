#![cfg(target_arch = "wasm32")]

pub(crate) mod host_abi;
mod events;
mod verbs;

pub use host_abi::{
    host_fs_read, host_fs_write, host_fs_readdir, host_fs_stat,
    host_fetch, host_kv_get, host_kv_put, host_kv_delete, host_kv_query,
    host_vec_search, host_vec_embed, host_exec_js, host_log, host_now_ms,
    host_env_get, host_browser_exec, host_task_proc, host_git,
    host_plugin_call,
    host_task, git_call, git_porcelain, git_call_argv, plugin_call,
    unpack_to_value_pub, unpack_to_string_pub, pack_ptr_len_pub,
    host_read, host_write, host_stat, host_exists, host_remove_file_never_directory,
    host_kv_read, host_cwd_string,
    host_cas_write,
};
pub(crate) use events::{current_dispatch_session_id, emit_event};
pub use verbs::{memory_recall_backend, route_hint, vec_search_local, embed_query};
pub use verbs::{
    ERR_CODE_FAILED, ERR_CODE_GATE_DENIED, ERR_CODE_INVALID_ARGS, ERR_CODE_PANIC,
    ERR_CODE_RETIRED_VERB, ERR_CODE_UNKNOWN_VERB, ERR_CODE_UNSUPPORTED,
    PLUGIN_FAIL_DEADLINE, PLUGIN_FAIL_HOST_EMPTY, PLUGIN_FAIL_MALFORMED,
    PLUGIN_FAIL_NOT_LOADED, PLUGIN_FAIL_PLUGIN_ERROR, PLUGIN_FAIL_UNKNOWN_PLUGIN,
    plugin_failure_code, plugin_ok, plugin_error_detail,
};
pub use verbs::dispatch_verb;
