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

pub const FS_VERBS: &[&str] = &["fs_read", "fs_write", "fs_readdir", "fs_stat", "fetch", "env_get", "kv_get", "kv_put", "kv_query"];
pub const GIT_VERBS: &[&str] = &["git_status", "branch_status", "git_push", "git_add", "git_commit", "git_finalize", "git_log", "git_diff", "git_show", "git_fetch", "git_branch", "git_checkout", "git_merge", "git_merge_abort", "git_branch_delete", "git_rm", "git_revert", "git_reset"];
pub const SQL_VERBS: &[&str] = &["sql_open", "sql_close", "sql_list_dbs", "sql_exec", "sql_query", "sql_smoke", "sql_serialize", "sql_deserialize"];
pub const MEMORY_VERBS: &[&str] = &["memorize", "memorize-prune", "recall", "codeinsight_index", "codesearch", "forget", "discipline"];
pub const EXEC_VERBS: &[&str] = &["exec_js", "lang", "python", "bash", "powershell", "ssh", "go", "rust", "c", "cpp", "java", "deno"];
pub const BROWSER_VERBS: &[&str] = &["browser"];
pub const META_VERBS: &[&str] = &["health", "config_resolve", "dataflow_resolve", "status", "close", "filter", "cache_get", "cache_put", "cache_invalidate", "cache_stats", "learn"];

/// The verb alias table, declared instead of being implicit in
/// `dispatch_verb_inner`'s `|`-joined match arms. Each entry is
/// (alias, canonical). `health` advertises this so a caller can tell that
/// `js` and `exec_js` are the same verb without reading the dispatch source.
///
/// The `lang_is_preserved` flag records the asymmetry the alias arms encode:
/// the shell family collapses every alias onto the literal "bash" before
/// reaching `shell_exec`, while the compiled-language family forwards the
/// dispatched verb itself, so `rust` and `cpp` survive as the lang argument.
///
/// `SELF_LANG_VERBS` is therefore deliberately NOT in this table: those verbs
/// share one match arm with `go` but are not aliases of it -- each reaches
/// `shell_exec` as its own lang. Listing them as aliases would claim `rust`
/// runs as `go`, which is the opposite of what dispatch does.
pub const VERB_ALIASES: &[(&str, &str)] = &[
    ("nodejs", "exec_js"),
    ("javascript", "exec_js"),
    ("node", "exec_js"),
    ("js", "exec_js"),
    ("memorize_prune", "memorize-prune"),
    ("py", "python"),
    ("sh", "bash"),
    ("shell", "bash"),
    ("zsh", "bash"),
    ("ps1", "powershell"),
];

pub const SELF_LANG_VERBS: &[&str] = &["go", "rust", "c", "cpp", "java", "deno"];

pub fn canonical_verb(verb: &str) -> &str {
    VERB_ALIASES.iter()
        .find(|(alias, _)| *alias == verb)
        .map(|(_, canonical)| *canonical)
        .unwrap_or(verb)
}

pub fn alias_table() -> Vec<(&'static str, &'static str, bool)> {
    VERB_ALIASES.iter()
        .map(|(alias, canonical)| {
            let lang_is_preserved = !matches!(*canonical, "bash" | "python" | "powershell");
            (*alias, *canonical, lang_is_preserved)
        })
        .collect()
}

pub fn all_verbs_by_subsystem() -> Vec<(Subsystem, &'static [&'static str])> {
    vec![
        (Subsystem::Fs, FS_VERBS),
        (Subsystem::Git, GIT_VERBS),
        (Subsystem::Sql, SQL_VERBS),
        (Subsystem::Memory, MEMORY_VERBS),
        (Subsystem::Exec, EXEC_VERBS),
        (Subsystem::Browser, BROWSER_VERBS),
        (Subsystem::Orchestrator, crate::orchestrator::ORCHESTRATOR_VERBS),
        (Subsystem::Meta, META_VERBS),
    ]
}
