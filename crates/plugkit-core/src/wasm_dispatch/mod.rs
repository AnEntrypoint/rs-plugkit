#![cfg(target_arch = "wasm32")]

mod host_abi;
mod events;
mod verbs;

pub use host_abi::{
    host_fs_read, host_fs_write, host_fs_readdir, host_fs_stat,
    host_fetch, host_kv_get, host_kv_put, host_kv_delete, host_kv_query,
    host_vec_search, host_vec_embed, host_exec_js, host_log, host_now_ms,
    host_env_get, host_browser_exec, host_task_proc, host_git,
    host_task, git_call, git_porcelain, git_call_argv,
    unpack_to_value_pub, unpack_to_string_pub,
    host_read, host_write, host_stat, host_exists, host_remove,
    host_kv_read,
};
pub(crate) use events::emit_event;
pub use verbs::{memory_recall_backend, route_hint};
pub use verbs::dispatch_verb;
