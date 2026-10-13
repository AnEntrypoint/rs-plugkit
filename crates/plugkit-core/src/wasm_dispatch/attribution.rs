use crate::pkfs;
use serde_json::{json, Map, Value};

const POLICY_FILE_REL: &str = ".gm/attribution.json";
const ACCEPTED_KEYS: &str = "mode, author";
const ACCEPTED_MODES: &str = "off, strip, refuse";

pub const ERR_CODE_ATTRIBUTION_REFUSED: &str = "attribution_refused";
pub const ERR_CODE_ATTRIBUTION_POLICY_INVALID: &str = "attribution_policy_invalid";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Off,
    Strip,
    Refuse,
}

pub struct Policy {
    pub mode: Mode,
    pub author: Option<(String, String)>,
}

pub struct Screened {
    pub text: String,
    pub stripped: Vec<String>,
}

impl Policy {
    fn off() -> Policy {
        Policy {
            mode: Mode::Off,
            author: None,
        }
    }

    pub fn enforces(&self) -> bool {
        self.mode != Mode::Off
    }
}

fn author_of(object: &Map<String, Value>, source: &str) -> Result<Option<(String, String)>, String> {
    let Some(raw) = object.get("author") else {
        return Ok(None);
    };
    let field = |key: &str| {
        raw.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    match (field("name"), field("email")) {
        (Some(name), Some(email)) => Ok(Some((name, email))),
        _ => Err(format!(
            "{source}: author must be an object with a nonempty name and a nonempty email"
        )),
    }
}

fn parse(text: &str, source: &str) -> Result<Policy, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("{source}: not valid JSON ({error})"))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("{source}: expected a JSON object with the keys {ACCEPTED_KEYS}"))?;
    if let Some(unknown) = object.keys().find(|key| !matches!(key.as_str(), "mode" | "author")) {
        return Err(format!("{source}: unknown key {unknown:?}; accepted keys: {ACCEPTED_KEYS}"));
    }
    let mode = match object.get("mode") {
        None => Mode::Off,
        Some(raw) => match raw.as_str().map(|mode| mode.trim().to_ascii_lowercase()).as_deref() {
            Some("off") => Mode::Off,
            Some("strip") => Mode::Strip,
            Some("refuse") => Mode::Refuse,
            _ => return Err(format!("{source}: mode must be one of {ACCEPTED_MODES}, got {raw}")),
        },
    };
    Ok(Policy {
        mode,
        author: author_of(object, source)?,
    })
}

fn policy_paths() -> Vec<String> {
    let mut paths = vec![pkfs::anchor(POLICY_FILE_REL)];
    if let Some(home) = crate::config::home_dir() {
        paths.push(format!("{}/{POLICY_FILE_REL}", home.trim_end_matches(['/', '\\'])));
    }
    paths
}

pub fn load() -> Result<Policy, String> {
    for path in policy_paths() {
        if let Some(text) = pkfs::read_to_string(&path) {
            return parse(&text, &path);
        }
    }
    Ok(Policy::off())
}

pub fn is_attribution_line(line: &str) -> bool {
    let lower = line.trim().to_ascii_lowercase();
    let trailer = lower.starts_with("co-authored-by:")
        && (lower.contains("claude") || lower.contains("anthropic"));
    trailer || (lower.contains("generated with") && lower.contains("claude"))
}

pub fn screen(message: &str) -> Screened {
    let mut kept: Vec<&str> = Vec::new();
    let mut stripped: Vec<String> = Vec::new();
    let mut after_removal = false;
    for line in message.lines() {
        if is_attribution_line(line) {
            stripped.push(line.trim().to_string());
            after_removal = true;
            continue;
        }
        let blank = line.trim().is_empty();
        if after_removal && blank && kept.last().map_or(true, |previous| previous.trim().is_empty()) {
            continue;
        }
        if !blank {
            after_removal = false;
        }
        kept.push(line);
    }
    if stripped.is_empty() {
        return Screened {
            text: message.to_string(),
            stripped,
        };
    }
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    Screened {
        text: kept.join("\n"),
        stripped,
    }
}

pub fn apply(verb: &str, message: &str, policy: &Policy) -> Result<Screened, Value> {
    if !policy.enforces() {
        return Ok(Screened {
            text: message.to_string(),
            stripped: Vec::new(),
        });
    }
    let screened = screen(message);
    if screened.stripped.is_empty() {
        return Ok(screened);
    }
    if policy.mode == Mode::Refuse {
        return Err(json!({
            "error": format!("{verb} refused: the message carries {} attribution line(s) this project's policy forbids", screened.stripped.len()),
            "error_code": ERR_CODE_ATTRIBUTION_REFUSED,
            "attribution_lines": screened.stripped,
            "hint": "remove those lines and dispatch again; the policy lives in .gm/attribution.json (project) or ~/.gm/attribution.json (user), key mode",
        }));
    }
    if screened.text.trim().is_empty() {
        return Err(json!({
            "error": format!("{verb} refused: the message held only attribution lines, so nothing is left to commit with"),
            "error_code": ERR_CODE_ATTRIBUTION_REFUSED,
            "attribution_lines": screened.stripped,
        }));
    }
    Ok(screened)
}

pub fn invalid(reason: &str) -> Value {
    json!({
        "error": format!("attribution policy rejected: {reason}"),
        "error_code": ERR_CODE_ATTRIBUTION_POLICY_INVALID,
        "hint": "fix or delete that file; an unreadable policy refuses the commit rather than silently turning enforcement off",
    })
}
