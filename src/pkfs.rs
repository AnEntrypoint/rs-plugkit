#[cfg(not(target_arch = "wasm32"))]
pub fn read_to_string(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn write(path: &str, data: &str) -> bool {
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, data).is_ok()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn exists(path: &str) -> bool {
    std::path::Path::new(path).exists()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn remove(path: &str) -> bool {
    std::fs::remove_file(path).is_ok()
}

#[cfg(target_arch = "wasm32")]
pub fn read_to_string(path: &str) -> Option<String> {
    crate::wasm_dispatch::host_read(path)
}

#[cfg(target_arch = "wasm32")]
pub fn write(path: &str, data: &str) -> bool {
    crate::wasm_dispatch::host_write(path, data)
}

#[cfg(target_arch = "wasm32")]
pub fn exists(path: &str) -> bool {
    crate::wasm_dispatch::host_exists(path)
}

#[cfg(target_arch = "wasm32")]
pub fn remove(_path: &str) -> bool {
    false
}
