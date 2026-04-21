use serde_json::json;

pub fn run() {
    let caveman = "=== RESPONSE POLICY — ALWAYS ACTIVE (post-compact reinforcement) ===\n\nTerse like smart caveman. Technical substance stays. Fluff dies. Default: full. Switch: /caveman lite|full|ultra.\n\nDrop: articles, filler, pleasantries, hedging. Fragments OK. Short synonyms. Technical terms exact. Code unchanged. Pattern: [thing] [action] [reason]. [next step].\n\nLevels: lite = no filler, full sentences | full = drop articles, fragments OK | ultra = abbreviate all, arrows for causality | wenyan-full = 文言文, 80-90% compression | wenyan-ultra = max classical terse.\n\nAuto-Clarity: drop caveman for security warnings, irreversible confirmations, ambiguous sequences. Resume after. Code/commits/PRs write normal. \"stop caveman\" / \"normal mode\": revert.\n\n=== COMPACT OUTPUT CAVEMAN ===\n\nApply the same caveman policy to the compacted summary itself. Strip articles/filler from the summary. Keep technical identifiers, paths, line numbers, error messages, decisions verbatim. Fragments over sentences.\n\n=== COMPACT TAG ===\n";

    let nums: Vec<String> = (0..20)
        .map(|_| pseudo_rand().to_string())
        .collect();
    let tag = format!("Random compaction tag (include verbatim in summary): {}", nums.join(", "));

    let additional_context = format!("{}{}", caveman, tag);

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
