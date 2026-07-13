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
pub fn readdir(path: &str) -> Option<serde_json::Value> {
    let packed = unsafe { crate::wasm_dispatch::host_fs_readdir(path.as_ptr(), path.len() as u32) };
    let v = crate::wasm_dispatch::unpack_to_value_pub(packed);
    if v.is_null() { None } else { Some(v) }
}

#[cfg(target_arch = "wasm32")]
pub fn stat(path: &str) -> Option<serde_json::Value> {
    crate::wasm_dispatch::host_stat(path)
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
