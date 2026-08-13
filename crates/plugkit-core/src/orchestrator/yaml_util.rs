use serde_yaml::Value;

pub fn levenshtein(a: &str, b: &str) -> usize {
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    let m = av.len();
    let n = bv.len();
    if m == 0 { return n; }
    if n == 0 { return m; }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut cur: Vec<usize> = vec![0; n + 1];
    for i in 1..=m {
        cur[0] = i;
        for j in 1..=n {
            let cost = if av[i - 1] == bv[j - 1] { 0 } else { 1 };
            cur[j] = (cur[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[n]
}

const B64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(B64_CHARS[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64_CHARS[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 { B64_CHARS[((n >> 6) & 0x3f) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64_CHARS[(n & 0x3f) as usize] as char } else { '=' });
    }
    out
}

fn base64_char_value(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some((c - b'A') as u32),
        b'a'..=b'z' => Some((c - b'a' + 26) as u32),
        b'0'..=b'9' => Some((c - b'0' + 52) as u32),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let clean: Vec<u8> = s.bytes().filter(|&c| c != b'=' && !c.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(clean.len() / 4 * 3 + 3);
    for chunk in clean.chunks(4) {
        let mut n: u32 = 0;
        let mut bits = 0u32;
        for &c in chunk {
            let v = base64_char_value(c).ok_or_else(|| "invalid base64 char".to_string())?;
            n = (n << 6) | v;
            bits += 6;
        }
        n <<= 24u32.saturating_sub(bits);
        let nbytes = (bits / 8) as usize;
        let b = n.to_be_bytes();
        out.extend_from_slice(&b[..nbytes]);
    }
    Ok(out)
}

pub fn invalidate_residual_marker() {
    let marker = super::gm_dir().join("residual-check-fired");
    let marker_s = marker.to_string_lossy().to_string();
    if crate::pkfs::exists(&marker_s) {
        let _ = crate::pkfs::write(&marker_s, "");
    }
}

pub fn yaml_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Number(n) => serde_json::from_str(&n.to_string()).unwrap_or(serde_json::Value::Null),
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Sequence(s) => serde_json::Value::Array(s.iter().map(yaml_to_json).collect()),
        Value::Mapping(m) => {
            let mut o = serde_json::Map::new();
            for (k, val) in m {
                if let Some(ks) = k.as_str() {
                    o.insert(ks.to_string(), yaml_to_json(val));
                }
            }
            serde_json::Value::Object(o)
        }
        Value::Tagged(t) => yaml_to_json(&t.value),
    }
}
