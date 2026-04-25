use serde_json::json;
use std::{env, fs, io::Read, path::Path, process::Command};

fn write_needs_gm() {
    let project_dir = env::var("CLAUDE_PROJECT_DIR")
        .unwrap_or_else(|_| env::current_dir().unwrap_or_default().to_string_lossy().to_string());
    if project_dir.is_empty() { return; }
    let gm_dir = Path::new(&project_dir).join(".gm");
    let _ = fs::create_dir_all(&gm_dir);
    let _ = fs::write(gm_dir.join("needs-gm"), "1");
}

fn extract_search_query(summary: &str) -> String {
    let words: Vec<&str> = summary
        .split_whitespace()
        .filter(|w| w.len() > 4 && w.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-'))
        .take(6)
        .collect();
    words.join(" ")
}

fn run_codeinsight(project_dir: &str) -> String {
    let bin = super::plugkit_bin();
    let out = Command::new(&bin)
        .args(["codeinsight", project_dir])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

fn run_search(project_dir: &str, query: &str) -> String {
    if query.is_empty() { return String::new(); }
    let bin = super::plugkit_bin();
    let out = Command::new(&bin)
        .args(["search", "--path", project_dir, query])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

pub fn run() {
    let mut stdin_buf = String::new();
    let _ = std::io::stdin().read_to_string(&mut stdin_buf);
    let stdin_data: serde_json::Value = serde_json::from_str(&stdin_buf).unwrap_or_default();
    let summary = stdin_data["summary"].as_str().unwrap_or("");

    write_needs_gm();
    let policy = super::load_prompt("pre-compact").unwrap_or_else(|| "=== RESPONSE POLICY — ALWAYS ACTIVE (post-compact reinforcement) ===\n\nTerse. Technical substance stays. Fluff dies. Drop: articles, filler, pleasantries, hedging. Fragments OK. Short synonyms. Technical terms exact. Code unchanged. Pattern: [thing] [action] [reason]. [next step].\n\nAuto-Clarity: drop terse style for security warnings, irreversible confirmations, ambiguous sequences. Resume after. Code/commits/PRs write normal.\n\n=== COMPACT OUTPUT STYLE ===\n\nApply the same terse policy to the compacted summary itself. Strip articles/filler from the summary. Keep technical identifiers, paths, line numbers, error messages, decisions verbatim. Fragments over sentences.\n\n=== POST-COMPACT FIRST RESPONSE — HARD RULE ===\n\nThe very next response after this compaction MUST call Skill(\"gm:gm\") as the FIRST tool invocation. Zero text output before it. No narration, no summary, no acknowledgement. The pre-tool-use hook is already armed — any other tool fires first and gets blocked. Invoke Skill(\"gm:gm\") immediately, silently.\n\n=== MEMORIZE SELF-CHECK (mandatory before closing this response) ===\n\nScan the conversation above for every exec: output, code read, CI result, or user preference that resolved an unknown but was NOT followed by an Agent(subagent_type='memorize') call. For each one found:\n\n  Agent(subagent_type='memorize', model='haiku', run_in_background=true, prompt='## CONTEXT TO MEMORIZE\\n<fact>')\n\nSpawn ALL missed memorize calls NOW, in parallel, before this response closes. One call per fact. Missing one = memory leak = bug.\n\n=== COMPACT TAG ===".to_string());

    let nums: Vec<String> = (0..20)
        .map(|_| pseudo_rand().to_string())
        .collect();
    let tag = format!("Random compaction tag (include verbatim in summary): {}", nums.join(", "));

    let project_dir = env::var("CLAUDE_PROJECT_DIR")
        .unwrap_or_else(|_| env::current_dir().unwrap_or_default().to_string_lossy().to_string());

    let mut additional_context = format!("{}{}", policy, tag);

    let insight = run_codeinsight(&project_dir);
    if !insight.is_empty() {
        additional_context.push_str("\n\n=== CODEBASE INSIGHT (post-compact context) ===\n");
        additional_context.push_str(&insight);
    }

    let query = extract_search_query(summary);
    let search_results = run_search(&project_dir, &query);
    if !search_results.is_empty() {
        additional_context.push_str("\n\n=== CODE SEARCH (query: ");
        additional_context.push_str(&query);
        additional_context.push_str(") ===\n");
        additional_context.push_str(&search_results);
    }

    let output = json!({
        "systemMessage": additional_context
    });

    println!("{}", serde_json::to_string(&output).unwrap_or_default());
}

fn pseudo_rand() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let mut x = nanos ^ pid.wrapping_mul(2654435761);
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    std::thread::sleep(std::time::Duration::from_nanos(1));
    x % 1000
}
