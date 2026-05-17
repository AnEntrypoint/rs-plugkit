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

#[cfg(not(target_arch = "wasm32"))]
pub fn read_to_string(_path: &str) -> Option<String> { None }
#[cfg(not(target_arch = "wasm32"))]
pub fn write(_path: &str, _data: &str) -> bool { false }
#[cfg(not(target_arch = "wasm32"))]
pub fn exists(_path: &str) -> bool { false }
#[cfg(not(target_arch = "wasm32"))]
pub fn remove(_path: &str) -> bool { false }
