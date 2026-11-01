use serde_json::Value;

pub(crate) struct ProbeDir(String);

fn session_id_of(body: &str) -> Option<String> {
    let sid = serde_json::from_str::<Value>(body)
        .ok()?
        .get("session_id")?
        .as_str()?
        .to_owned();
    let plain = !sid.is_empty()
        && sid.len() <= 128
        && sid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    plain.then_some(sid)
}

fn submodule_checkout() -> bool {
    crate::pkfs::read_to_string(".git").is_some_and(|text| text.trim_start().starts_with("gitdir:"))
}

pub(crate) fn begin(body: impl FnOnce() -> String) -> Option<ProbeDir> {
    if !submodule_checkout() {
        return None;
    }
    let sid = session_id_of(&body())?;
    let dir = format!(".gm/dream-rsi/{sid}");
    if crate::pkfs::readdir(&dir).is_some() {
        return None;
    }
    Some(ProbeDir(dir))
}

const DISPATCH_TS_TEMP_PREFIX: &str = "last-dispatch-ts.tmp-";

pub(crate) fn end(probe: Option<ProbeDir>) {
    sweep_dispatch_ts_temps();
    if let Some(ProbeDir(dir)) = probe {
        crate::pkfs::remove_tree(&dir);
    }
}

fn sweep_dispatch_ts_temps() {
    let Some(Value::Array(entries)) = crate::pkfs::readdir(".gm") else {
        return;
    };
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(|n| n.as_str())
            .or_else(|| entry.as_str());
        if let Some(name) = name.filter(|n| n.starts_with(DISPATCH_TS_TEMP_PREFIX)) {
            crate::pkfs::remove_file(&format!(".gm/{name}"));
        }
    }
}
