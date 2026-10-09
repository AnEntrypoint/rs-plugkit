#![cfg(target_arch = "wasm32")]

pub(crate) mod host_abi;
mod dangling_refs;
mod events;
mod verbs;

pub(crate) use events::{current_dispatch_session_id, emit_event};
pub use host_abi::{
    git_call, git_call_argv, git_porcelain, host_allow_root, host_cas_write,
    host_cwd_string, host_env_get, host_exec_js, host_exists, host_fetch, host_fs_read,
    host_fs_readdir, host_fs_stat, host_fs_write, host_git, host_kv_delete, host_kv_get,
    host_kv_put, host_kv_query, host_kv_read, host_log, host_now_ms, host_plugin_call, host_read,
    host_remove_file_never_directory, host_stat, host_stat_is_directory, host_task,
    host_task_proc, host_vec_embed,
    host_vec_search, host_write, pack_ptr_len_pub, plugin_call, unpack_to_string_pub,
    unpack_to_value_pub,
};
pub(crate) use verbs::GIT_PROTECTED_PATHSPECS;
pub use verbs::dispatch_verb;
pub use verbs::{embed_query, memory_recall_backend, route_hint, vec_search_local};
pub use verbs::{
    plugin_error_detail, plugin_failure_code, plugin_ok, ERR_CODE_FAILED, ERR_CODE_GATE_DENIED,
    ERR_CODE_INVALID_ARGS, ERR_CODE_PANIC, ERR_CODE_RETIRED_VERB, ERR_CODE_UNKNOWN_VERB,
    ERR_CODE_UNSUPPORTED, PLUGIN_FAIL_DEADLINE, PLUGIN_FAIL_HOST_EMPTY, PLUGIN_FAIL_MALFORMED,
    PLUGIN_FAIL_NOT_LOADED, PLUGIN_FAIL_PLUGIN_ERROR, PLUGIN_FAIL_UNKNOWN_PLUGIN,
};
