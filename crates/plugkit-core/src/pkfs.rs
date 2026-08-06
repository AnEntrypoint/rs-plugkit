#[cfg(target_arch = "wasm32")]
pub fn anchor(path: &str) -> String {
    if is_absolute(path) {
        return path.to_string();
    }
    let rel = path.trim_start_matches("./");
    match project_root() {
        Some(root) => format!("{}/{}", root, rel),
        None => path.to_string(),
    }
}

#[cfg(target_arch = "wasm32")]
static ROOT_CACHE: std::sync::Mutex<Option<std::collections::HashMap<String, String>>> =
    std::sync::Mutex::new(None);

#[cfg(target_arch = "wasm32")]
fn project_root() -> Option<String> {
    let cwd = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
    let cwd = cwd.trim_end_matches(['/', '\\']).to_string();
    if let Ok(cache) = ROOT_CACHE.lock() {
        if let Some(root) = cache.as_ref().and_then(|m| m.get(&cwd)) {
            return Some(root.clone());
        }
    }
    let root = match git_toplevel() {
        Some(r) => r,
        None if cwd.is_empty() => return None,
        None => cwd.clone(),
    };
    if let Ok(mut cache) = ROOT_CACHE.lock() {
        cache.get_or_insert_with(std::collections::HashMap::new).insert(cwd, root.clone());
    }
    Some(root)
}

#[cfg(target_arch = "wasm32")]
fn git_toplevel() -> Option<String> {
    let v = crate::wasm_dispatch::git_call("rev-parse --show-toplevel", None);
    let out = v.get("stdout").and_then(|x| x.as_str())?;
    let top = out.lines().next()?.trim().trim_end_matches(['/', '\\']);
    if top.is_empty() { None } else { Some(top.to_string()) }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn anchor(path: &str) -> String {
    path.to_string()
}

pub fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || path.starts_with('\\') || path.chars().nth(1) == Some(':')
}

#[cfg(target_arch = "wasm32")]
pub fn read_to_string(path: &str) -> Option<String> {
    crate::wasm_dispatch::host_read(&anchor(path))
}

#[cfg(target_arch = "wasm32")]
pub fn write(path: &str, data: &str) -> bool {
    crate::wasm_dispatch::host_write(&anchor(path), data)
}

#[cfg(target_arch = "wasm32")]
pub fn exists(path: &str) -> bool {
    crate::wasm_dispatch::host_exists(&anchor(path))
}

#[cfg(target_arch = "wasm32")]
pub fn readdir(path: &str) -> Option<serde_json::Value> {
    let anchored = anchor(path);
    let packed = unsafe {
        crate::wasm_dispatch::host_fs_readdir(anchored.as_ptr(), anchored.len() as u32)
    };
    let v = crate::wasm_dispatch::unpack_to_value_pub(packed);
    if v.is_null() { None } else { Some(v) }
}

#[cfg(target_arch = "wasm32")]
pub fn stat(path: &str) -> Option<serde_json::Value> {
    crate::wasm_dispatch::host_stat(&anchor(path))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn read_to_string(_path: &str) -> Option<String> { None }
#[cfg(not(target_arch = "wasm32"))]
pub fn write(_path: &str, _data: &str) -> bool { false }
#[cfg(not(target_arch = "wasm32"))]
pub fn exists(_path: &str) -> bool { false }
#[cfg(not(target_arch = "wasm32"))]
pub fn readdir(_path: &str) -> Option<serde_json::Value> { None }
#[cfg(not(target_arch = "wasm32"))]
pub fn stat(_path: &str) -> Option<serde_json::Value> { None }
