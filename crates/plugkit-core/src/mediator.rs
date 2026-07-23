#![cfg(target_arch = "wasm32")]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subsystem {
    Fs,
    Git,
    Sql,
    Memory,
    Exec,
    Browser,
    Orchestrator,
    Meta,
}

impl Subsystem {
    pub fn as_str(&self) -> &'static str {
        match self {
            Subsystem::Fs => "fs",
            Subsystem::Git => "git",
            Subsystem::Sql => "sql",
            Subsystem::Memory => "memory",
            Subsystem::Exec => "exec",
            Subsystem::Browser => "browser",
            Subsystem::Orchestrator => "orchestrator",
            Subsystem::Meta => "meta",
        }
    }
}

pub fn all_verbs_by_subsystem() -> Vec<(Subsystem, &'static [&'static str])> {
    vec![
        (Subsystem::Fs, &["fs_read", "fs_write", "fs_readdir", "fs_stat", "fetch", "env_get", "kv_get", "kv_put", "kv_query"]),
        (Subsystem::Git, &["git_status", "branch_status", "git_push", "git_add", "git_commit", "git_finalize", "git_log", "git_diff", "git_show", "git_fetch", "git_branch", "git_checkout", "git_rm", "git_revert", "git_reset"]),
        (Subsystem::Sql, &["sql_open", "sql_close", "sql_list_dbs", "sql_exec", "sql_query", "sql_smoke", "sql_serialize", "sql_deserialize"]),
        (Subsystem::Memory, &["memorize", "memorize-prune", "recall", "codeinsight_index", "codesearch", "forget", "discipline"]),
        (Subsystem::Exec, &["exec_js", "lang", "python", "bash", "powershell", "ssh"]),
        (Subsystem::Browser, &["browser"]),
    ]
}
