use std::fs;
use std::path::PathBuf;
use super::gm_dir;

pub fn memorize_inbox() -> PathBuf {
    gm_dir().join("exec-spool").join("in").join("memorize")
}

pub fn fire(body: &str) -> Result<String, std::io::Error> {
    let dir = memorize_inbox();
    fs::create_dir_all(&dir)?;
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let p = dir.join(format!("{}.md", n));
    fs::write(&p, body)?;
    Ok(p.display().to_string())
}

pub fn handle_fire(content: &str) -> (String, String, i32) {
    if content.trim().is_empty() {
        return (String::new(), "empty memorize body".to_string(), 1);
    }
    match fire(content) {
        Ok(path) => {
            let payload = serde_json::json!({ "spooled": path });
            (payload.to_string(), String::new(), 0)
        }
        Err(e) => (String::new(), format!("memorize fire failed: {}", e), 1),
    }
}
