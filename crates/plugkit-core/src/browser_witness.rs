#![cfg(target_arch = "wasm32")]

use serde_json::{json, Value};
use crate::wasm_dispatch::{host_read, host_write, host_log};

const BROWSER_DIR_PREFIXES: &[&str] = &["public/", "site/", "app/", "pages/", "components/", "client/", "web/", "src/frontend/", "packages/web-app/", "frontend/", "webapp/"];

fn lower_ends_with(s: &str, ext: &str) -> bool {
    let ll = s.len();
    let el = ext.len();
    if ll < el { return false; }
    s[ll - el..].eq_ignore_ascii_case(ext)
}

pub fn is_browser_running_file(rel: &str) -> bool {
    if rel.is_empty() { return false; }
    let norm = rel.replace('\\', "/");
    for ext in &[".html", ".htm", ".tsx", ".jsx", ".vue", ".svelte"] {
        if lower_ends_with(&norm, ext) { return true; }
    }
    let is_codey = [".mjs", ".cjs", ".js", ".ts", ".css", ".scss", ".sass"]
        .iter().any(|e| lower_ends_with(&norm, e));
    if !is_codey { return false; }
    let lower = norm.to_lowercase();
    BROWSER_DIR_PREFIXES.iter().any(|p| lower.starts_with(p))
}

fn relpath(cwd: &str, abs: &str) -> String {
    let cwd_n = cwd.replace('\\', "/");
    let abs_n = abs.replace('\\', "/");
    let cwd_t = cwd_n.trim_end_matches('/');
    if !cwd_t.is_empty() && abs_n.starts_with(cwd_t) {
        let rest = &abs_n[cwd_t.len()..];
        rest.trim_start_matches('/').to_string()
    } else {
        abs_n
    }
}

const SHA256_K: [u32; 64] = [
    0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
    0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
    0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
    0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
    0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
    0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
    0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
    0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2,
];

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,
        0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19,
    ];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded = data.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 { padded.push(0); }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in padded.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[i*4], chunk[i*4+1], chunk[i*4+2], chunk[i*4+3]]);
        }
        for i in 16..64 {
            let s0 = w[i-15].rotate_right(7) ^ w[i-15].rotate_right(18) ^ (w[i-15] >> 3);
            let s1 = w[i-2].rotate_right(17) ^ w[i-2].rotate_right(19) ^ (w[i-2] >> 10);
            w[i] = w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1);
        }
        let (mut a,mut b,mut c,mut d,mut e,mut f,mut g,mut hh) = (h[0],h[1],h[2],h[3],h[4],h[5],h[6],h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(SHA256_K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let mj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(mj);
            hh = g; g = f; f = e; e = d.wrapping_add(t1); d = c; c = b; b = a; a = t1.wrapping_add(t2);
        }
        h[0]=h[0].wrapping_add(a); h[1]=h[1].wrapping_add(b); h[2]=h[2].wrapping_add(c); h[3]=h[3].wrapping_add(d);
        h[4]=h[4].wrapping_add(e); h[5]=h[5].wrapping_add(f); h[6]=h[6].wrapping_add(g); h[7]=h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for i in 0..8 { out[i*4..i*4+4].copy_from_slice(&h[i].to_be_bytes()); }
    out
}

fn sha256_hex_first32(data: &[u8]) -> String {
    let hash = sha256(data);
    let mut s = String::with_capacity(32);
    for b in hash.iter().take(16) {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn hash_file_short(rel: &str) -> String {
    match host_read(rel) {
        Some(content) => sha256_hex_first32(content.as_bytes()),
        None => String::new(),
    }
}

fn now_ms() -> u64 {
    unsafe { crate::wasm_dispatch::host_now_ms() }
}

fn log_warn(msg: &str) {
    unsafe { host_log(2, msg.as_ptr(), msg.len() as u32); }
}

pub fn record_edit(cwd: &str, file_path: &str) -> Result<bool, String> {
    let rel = if cwd.is_empty() { file_path.to_string() } else { relpath(cwd, file_path) };
    let rel_slash = rel.replace('\\', "/");
    if !is_browser_running_file(&rel_slash) { return Ok(false); }

    let edits_path = if cwd.is_empty() {
        ".gm/exec-spool/.turn-browser-edits.json".to_string()
    } else {
        format!("{}/.gm/exec-spool/.turn-browser-edits.json", cwd.trim_end_matches('/').trim_end_matches('\\'))
    };

    let existing = host_read(&edits_path).unwrap_or_default();
    let mut list: Vec<Value> = if existing.is_empty() {
        vec![]
    } else {
        serde_json::from_str::<Value>(&existing)
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    };

    let hash = hash_file_short(&rel_slash);
    let entry = json!({ "file": rel_slash, "ts": now_ms(), "hash": hash });

    let mut found = false;
    for e in list.iter_mut() {
        if e.get("file").and_then(|f| f.as_str()) == Some(rel_slash.as_str()) {
            *e = entry.clone();
            found = true;
            break;
        }
    }
    if !found { list.push(entry); }

    let serialized = Value::Array(list).to_string();
    if !host_write(&edits_path, &serialized) {
        log_warn(&format!("browser_witness: write failed for {}", edits_path));
        return Err(format!("host_fs_write failed for {}", edits_path));
    }
    Ok(true)
}

pub fn record_from_body(cwd: &str, body: &Value) {
    for k in &["file_path", "filePath", "path"] {
        if let Some(p) = body.get(*k).and_then(|v| v.as_str()) {
            if p.is_empty() { continue; }
            let _ = record_edit(cwd, p);
        }
    }
}
