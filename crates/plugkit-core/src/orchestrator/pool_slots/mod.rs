mod admission;
mod brief;
mod floor;
mod heartbeats;
mod observe;
mod state;
mod traversal;

pub use admission::slots_prose;
pub use brief::handle_brief;
pub use floor::{floor_gate, monitor_block};
pub use heartbeats::{HEARTBEAT_LIVE_MS, HEARTBEAT_REFRESH_MS};
pub use observe::handle_observe;
pub use state::{slot_state, spawn_ceiling};

const DEFAULT_SPAWN_CEILING: usize = 20;
const REFILL_FLOOR: u64 = 10;
const LAUNCH_ID_PREFIX: &str = "witness-gap-";
const TRAVERSAL_LAUNCH_ID: &str = "traversal-node-supply";
const MODULE_EXTENSIONS: [&str; 6] = [".js", ".mjs", ".cjs", ".ts", ".jsx", ".tsx"];

#[cfg(target_arch = "wasm32")]
fn now_ms() -> u64 {
    unsafe { crate::wasm_dispatch::host_now_ms() }
}

#[cfg(not(target_arch = "wasm32"))]
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn pool_dir(project_root: &str) -> String {
    format!("{}/.gm/pool", project_root)
}
