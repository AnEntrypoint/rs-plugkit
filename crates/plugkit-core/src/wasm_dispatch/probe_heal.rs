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

pub(crate) fn end(probe: Option<ProbeDir>) {
    if let Some(ProbeDir(dir)) = probe {
        crate::pkfs::remove_tree(&dir);
    }
}
