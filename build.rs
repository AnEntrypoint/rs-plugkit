use std::fs;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=Cargo.lock");
    let lock_path = Path::new("Cargo.lock");
    let text = fs::read_to_string(lock_path).unwrap_or_default();
    let targets = ["rs-exec", "rs-search", "rs-codeinsight"];
    for target in &targets {
        let env_name = format!("DEP_{}_SHA", target.replace('-', "_").to_uppercase());
        let sha = extract_sha(&text, target).unwrap_or_else(|| "unknown".to_string());
        println!("cargo:rustc-env={}={}", env_name, sha);
    }
}

fn extract_sha(lock: &str, crate_name: &str) -> Option<String> {
    let needle = format!("name = \"{}\"", crate_name);
    let idx = lock.find(&needle)?;
    let after = &lock[idx..];
    let end = after.find("\n\n").unwrap_or(after.len());
    let block = &after[..end];
    for line in block.lines() {
        if let Some(src) = line.strip_prefix("source = \"") {
            if let Some(hash_idx) = src.rfind('#') {
                let sha = &src[hash_idx + 1..src.len() - 1];
                return Some(sha.chars().take(12).collect());
            }
        }
    }
    None
}
