use yaml_serde::Value;

pub fn levenshtein(a: &str, b: &str) -> usize {
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    let m = av.len();
    let n = bv.len();
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
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
        out.push(if chunk.len() > 1 {
            B64_CHARS[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_CHARS[(n & 0x3f) as usize] as char
        } else {
            '='
        });
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
    let clean: Vec<u8> = s
        .bytes()
        .filter(|&c| c != b'=' && !c.is_ascii_whitespace())
        .collect();
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

pub fn defer_marker_in_text(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    const HARD_MARKERS: &[&str] = &[
        "defer to later",
        "deferred to later",
        "deferred for later",
        "address it next",
        "address this next",
        "leave for next",
        "documented for next",
        "documented for future",
        "below criticality",
        "skip for now",
        "punt for now",
        "do later",
        "fix later",
        "later pass",
        "future work",
    ];
    for m in HARD_MARKERS {
        if lower.contains(m) {
            return Some(m);
        }
    }
    const SOFT_MARKERS: &[&str] = &[
        "next pass",
        "next session",
        "next turn",
        "future pass",
        "future session",
        "future turn",
    ];
    let has_soft = SOFT_MARKERS.iter().find(|m| lower.contains(**m)).copied();
    if let Some(m) = has_soft {
        const DEFER_CUE: &[&str] = &[
            "defer",
            "punt",
            "leave",
            "save it",
            "save this",
            "push to",
            "kick to",
            "hold for",
            "wait for",
            "do it in",
            "handle in",
            "finish in",
        ];
        const RETRO_CUE: &[&str] = &[
            "quoted",
            "described",
            "the phrase",
            "rejected for",
            "flagged",
            "caused by",
            "deviation",
            "false-positive",
            "false positive",
            "was mine",
            "self-caught",
            "tripped",
            "substring",
        ];
        let retro = RETRO_CUE.iter().any(|c| lower.contains(c));
        let defer = DEFER_CUE.iter().any(|c| lower.contains(c));
        if defer && !retro {
            return Some(m);
        }
    }
    None
}

pub const RESIDUAL_MARKER_NAME: &str = "residual-check-fired";

pub enum ResidualMarker {
    Absent,
    Live { session_id: String, fired_at_ms: u64 },
    Invalidated { reason: String },
    Malformed { raw: String },
}

pub fn residual_marker_path() -> String {
    super::gm_dir().join(RESIDUAL_MARKER_NAME).to_string_lossy().to_string()
}

pub fn read_residual_marker() -> ResidualMarker {
    let raw = match crate::pkfs::read_to_string(&residual_marker_path()) {
        Some(s) => s,
        None => return ResidualMarker::Absent,
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return ResidualMarker::Absent;
    }
    if let Some(reason) = trimmed.strip_prefix("invalidated:") {
        return ResidualMarker::Invalidated { reason: reason.to_string() };
    }
    let mut parts = trimmed.splitn(2, ':');
    let session_id = parts.next().unwrap_or("").to_string();
    match parts.next().and_then(|s| s.parse().ok()) {
        Some(fired_at_ms) => ResidualMarker::Live { session_id, fired_at_ms },
        None => ResidualMarker::Malformed { raw: trimmed.to_string() },
    }
}

pub fn invalidate_residual_marker(reason: &str) {
    let marker_s = residual_marker_path();
    if crate::pkfs::exists(&marker_s) {
        let _ = crate::pkfs::write(&marker_s, &format!("invalidated:{}", reason));
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamlFault {
    pub line: usize,
    pub column: usize,
    pub substring: String,
    pub token: String,
}

impl YamlFault {
    pub fn describe(&self) -> String {
        format!(
            "line {} column {}: {:?} (breaking token {:?})",
            self.line, self.column, self.substring, self.token
        )
    }
}

pub fn parse_fault(text: &str, err: &yaml_serde::Error) -> Option<YamlFault> {
    let (line, column) = error_position(err)
        .or_else(|| error_position_from_message(&err.to_string()))?;
    let line_text = text.split('\n').nth(line.saturating_sub(1))?;
    let start = column.saturating_sub(1).min(line_text.len());
    let tail = line_text.get(start..).unwrap_or("");
    let substring = truncate_chars(tail, 48);
    let token = breaking_token(tail);
    Some(YamlFault {
        line,
        column,
        substring,
        token,
    })
}

fn error_position(err: &yaml_serde::Error) -> Option<(usize, usize)> {
    let loc = err.location()?;
    Some((loc.line(), loc.column()))
}

fn error_position_from_message(message: &str) -> Option<(usize, usize)> {
    let bytes = message.as_bytes();
    let needle = b"at line ";
    let at = (0..=bytes.len().saturating_sub(needle.len()))
        .find(|&i| bytes[i..].starts_with(needle))?;
    let rest = &message[at + needle.len()..];
    let (line_text, tail) = split_number(rest)?;
    let after = tail.trim_start();
    let after = after.strip_prefix("column")?.trim_start();
    let (column_text, _) = split_number(after)?;
    Some((line_text.parse().ok()?, column_text.parse().ok()?))
}

fn split_number(s: &str) -> Option<(&str, &str)> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    Some((&s[..end], &s[end..]))
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

fn breaking_token(tail: &str) -> String {
    if let Some(rest) = tail.strip_prefix(':') {
        if rest.starts_with(' ') || rest.is_empty() {
            return ": ".to_string();
        }
    }
    if tail.contains(": ") {
        return ": ".to_string();
    }
    if tail.contains(" #") {
        return " #".to_string();
    }
    match tail.chars().next() {
        Some(c @ ('-' | '?' | ':' | '[' | '{' | '#' | '&' | '*' | '!' | '|' | '>' | '%' | '@'))
            if tail.len() > 1 && matches!(tail.chars().nth(1), Some(' ')) =>
        {
            format!("{} ", c)
        }
        Some(c) => c.to_string(),
        None => String::new(),
    }
}

pub fn needs_quoting(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    if value.trim() != value {
        return true;
    }
    if value
        .chars()
        .any(|c| matches!(c, '\n' | '\r' | '\t') || c.is_control())
    {
        return true;
    }
    let first = value.chars().next().unwrap_or(' ');
    if matches!(
        first,
        '-' | '?' | ':' | ',' | '[' | ']' | '{' | '}' | '#' | '&' | '*' | '!' | '|' | '>' | '\''
            | '"' | '%' | '@' | '`'
    ) {
        return true;
    }
    for (index, c) in value.char_indices() {
        match c {
            ':' if value[index + 1..].starts_with(' ') => return true,
            ':' if index + 1 == value.len() => return true,
            ' ' if value[index + 1..].starts_with('#') => return true,
            ' ' if index + 1 == value.len() => return true,
            _ => {}
        }
    }
    matches!(
        value.to_ascii_lowercase().as_str(),
        "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~" | ".nan" | ".inf" | "-.inf"
    )
}

pub fn quote_scalar(value: &str) -> String {
    if value
        .chars()
        .any(|c| matches!(c, '\n' | '\r' | '\t') || c.is_control())
    {
        let mut out = String::with_capacity(value.len() + 2);
        out.push('"');
        for c in value.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        return out;
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push_str("''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

pub fn serialize_checked(doc: &Value) -> Result<String, String> {
    let raw = yaml_serde::to_string(doc)
        .map_err(|e| format!("the updated document did not serialize ({e})"))?;
    let back: Value = yaml_serde::from_str(&raw).map_err(|e| {
        let where_at = parse_fault(&raw, &e)
            .map(|f| f.describe())
            .unwrap_or_else(|| "unknown position".to_string());
        format!("the serialized document does not re-parse ({e}) at {where_at}")
    })?;
    if &back != doc {
        return Err(
            "the serialized document does not round-trip: re-reading the emitted bytes yields a different document"
                .to_string(),
        );
    }
    Ok(raw)
}

pub fn requote_fault(text: &str, line: usize, column: usize) -> Option<(String, YamlFault)> {
    let lines: Vec<&str> = text.split('\n').collect();
    let fault_index = line.checked_sub(1)?;
    if fault_index >= lines.len() {
        return None;
    }
    let owner = owning_entry_index(&lines, fault_index)?;
    let entry = parse_entry(lines[owner])?;
    let mut end = owner;
    while end + 1 < lines.len() && is_continuation(lines[end + 1], entry.indent) {
        end += 1;
    }
    let mut value = entry.value.trim().to_string();
    for extra in lines[owner + 1..=end].iter() {
        let piece = extra.trim();
        if !piece.is_empty() {
            value.push(' ');
            value.push_str(piece);
        }
    }
    value = unwrap_quoted(&value);
    let rebuilt = format!(
        "{}{}{}: {}",
        entry.indent,
        entry.dash,
        entry.key,
        quote_scalar(&value)
    );
    let mut out_lines: Vec<String> = lines[..owner].iter().map(|l| (*l).to_string()).collect();
    out_lines.push(rebuilt);
    out_lines.extend(lines[end + 1..].iter().map(|l| (*l).to_string()));
    let out = out_lines.join("\n");
    let fault = YamlFault {
        line: owner + 1,
        column,
        substring: truncate_chars(&value, 48),
        token: breaking_token(&value),
    };
    Some((out, fault))
}

struct Entry<'a> {
    indent: &'a str,
    dash: &'a str,
    key: &'a str,
    value: &'a str,
}

fn parse_entry(line: &str) -> Option<Entry<'_>> {
    let body = line.trim_start_matches([' ', '\t']);
    let indent = &line[..line.len() - body.len()];
    let (dash, body) = match body.strip_prefix("- ") {
        Some(rest) => ("- ", rest),
        None => ("", body),
    };
    let bytes = body.as_bytes();
    let mut colon = None;
    for (index, &b) in bytes.iter().enumerate() {
        if b == b':' {
            let next = bytes.get(index + 1).copied();
            if next == Some(b' ') || next.is_none() {
                colon = Some(index);
                break;
            }
        }
    }
    let colon = colon?;
    let key = &body[..colon];
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
        || key.starts_with(|c: char| c.is_ascii_digit())
    {
        return None;
    }
    let rest = &body[colon + 1..];
    let value = rest.strip_prefix(' ').unwrap_or(rest);
    if value.trim().is_empty() {
        return None;
    }
    Some(Entry {
        indent,
        dash,
        key,
        value,
    })
}

fn is_mapping_entry(line: &str, indent_len: usize) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    line.len() - trimmed.len() >= indent_len && parse_entry(line).is_some()
}

fn is_continuation(line: &str, indent: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let own_indent = &line[..line.len() - line.trim_start_matches([' ', '\t']).len()];
    own_indent.len() > indent.len() && parse_entry(line).is_none()
}

fn owning_entry_index(lines: &[&str], fault_index: usize) -> Option<usize> {
    if let Some(entry) = parse_entry(lines[fault_index]) {
        if !entry.value.trim().is_empty() {
            return Some(fault_index);
        }
    }
    let mut index = fault_index;
    while index > 0 {
        index -= 1;
        let trimmed = lines[index].trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(entry) = parse_entry(lines[index]) {
            if !entry.value.trim().is_empty() {
                return Some(index);
            }
            return None;
        }
    }
    None
}

fn unwrap_quoted(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(inner) = trimmed
        .strip_prefix('\'')
        .and_then(|s| s.strip_suffix('\''))
    {
        return inner.replace("''", "'");
    }
    if let Some(inner) = trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return inner.replace("\\\"", "\"").replace("\\\\", "\\");
    }
    trimmed.to_string()
}

#[derive(Debug, Clone)]
pub struct RepairPlan {
    pub repaired: String,
    pub faults: Vec<YamlFault>,
    pub rows_after: usize,
    pub changed: bool,
}

pub fn repair_text(
    text: &str,
    max_passes: usize,
) -> Result<RepairPlan, (String, Vec<YamlFault>)> {
    let mut current = text.to_string();
    let mut faults: Vec<YamlFault> = Vec::new();
    if yaml_serde::from_str::<Value>(&current).is_ok() && !text.trim().is_empty() {
        let rows = row_count(&current);
        return Ok(RepairPlan {
            repaired: current,
            faults,
            rows_after: rows,
            changed: false,
        });
    }
    for _ in 0..max_passes.max(1) {
        let err = match yaml_serde::from_str::<Value>(&current) {
            Ok(_) => {
                let rows = row_count(&current);
                return Ok(RepairPlan {
                    repaired: current,
                    faults,
                    rows_after: rows,
                    changed: true,
                })
            }
            Err(e) => e,
        };
        let Some(fault) = parse_fault(&current, &err) else {
            return Err((
                format!("could not locate the parse fault ({err})"),
                faults,
            ));
        };
        let Some((next, fixed)) = requote_fault(&current, fault.line, fault.column) else {
            return Err((
                format!(
                    "the fault at {} is not inside a `key: value` entry, so it cannot be re-quoted automatically; repair those bytes by hand",
                    fault.describe()
                ),
                faults,
            ));
        };
        if next == current {
            return Err((
                format!(
                    "re-quoting made no progress at {}; refusing to write a half-repaired file",
                    fault.describe()
                ),
                faults,
            ));
        }
        faults.push(fixed);
        current = next;
    }
    Err((
        format!(
            "gave up after {} re-quote passes: {} faults were re-quoted ({}) and the document still does not parse",
            max_passes,
            faults.len(),
            faults.first().map(|f| f.describe()).unwrap_or_default()
        ),
        faults,
    ))
}

fn row_count(text: &str) -> usize {
    yaml_serde::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.as_sequence().map(|s| s.len()))
        .unwrap_or(0)
}
