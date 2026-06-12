use crate::pkfs;

pub fn resolve(key: &str, default: &str) -> String {
    let path = format!(".gm/instructions/{}.md", key);
    if let Some(raw) = pkfs::read_to_string(&path) {
        let text = raw.trim_start_matches('\u{feff}').replace("\r\n", "\n");
        if !text.trim().is_empty() {
            return text;
        }
    }
    default.to_string()
}
