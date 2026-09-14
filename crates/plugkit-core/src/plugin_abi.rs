#![cfg(target_arch = "wasm32")]

use serde_json::{json, Map, Value};

use crate::wasm_dispatch::plugin_call;

pub const ABI_VERSION: u64 = 1;

pub const KNOWN_PLUGINS: &[&str] = &["libsql", "bert", "treesitter"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiErrorKind {
    PluginNotFound,
    VerbNotSupported,
    PluginError,
    Timeout,
    Other(String),
}

impl AbiErrorKind {
    pub fn as_str(&self) -> &str {
        match self {
            AbiErrorKind::PluginNotFound => "plugin-not-found",
            AbiErrorKind::VerbNotSupported => "verb-not-supported",
            AbiErrorKind::PluginError => "plugin-error",
            AbiErrorKind::Timeout => "timeout",
            AbiErrorKind::Other(s) => s.as_str(),
        }
    }

    pub fn from_str(s: &str) -> AbiErrorKind {
        match s {
            "plugin-not-found" => AbiErrorKind::PluginNotFound,
            "verb-not-supported" => AbiErrorKind::VerbNotSupported,
            "plugin-error" => AbiErrorKind::PluginError,
            "timeout" => AbiErrorKind::Timeout,
            other => AbiErrorKind::Other(other.to_string()),
        }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(self, AbiErrorKind::Timeout)
    }
}

#[derive(Debug, Clone)]
pub struct AbiError {
    pub kind: AbiErrorKind,
    pub message: String,
    pub plugin: String,
    pub verb: String,
}

impl AbiError {
    pub fn new(kind: AbiErrorKind, plugin: &str, verb: &str, message: impl Into<String>) -> AbiError {
        AbiError { kind, message: message.into(), plugin: plugin.to_string(), verb: verb.to_string() }
    }

    pub fn to_response(&self) -> Value {
        json!({
            "ok": false,
            "abi": ABI_VERSION,
            "error": self.message,
            "kind": self.kind.as_str(),
            "plugin": self.plugin,
            "verb": self.verb,
        })
    }
}

impl std::fmt::Display for AbiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}: [{}] {}", self.plugin, self.verb, self.kind.as_str(), self.message)
    }
}

pub type AbiResult = Result<Value, AbiError>;

#[derive(Debug, Clone)]
pub struct Capability {
    pub plugin: String,
    pub abi: u64,
    pub verbs: Vec<String>,
}

impl Capability {
    pub fn supports(&self, verb: &str) -> bool {
        self.verbs.iter().any(|v| v == verb)
    }

    pub fn is_compatible(&self) -> bool {
        self.abi <= ABI_VERSION
    }

    pub fn to_response(&self) -> Value {
        json!({
            "ok": true,
            "abi": ABI_VERSION,
            "data": { "plugin": self.plugin, "abi": self.abi, "verbs": self.verbs },
        })
    }
}

pub fn request_envelope(plugin: &str, verb: &str, body: &Value) -> Value {
    json!({ "abi": ABI_VERSION, "plugin": plugin, "verb": verb, "body": body })
}

pub fn ok_response(data: Value) -> Value {
    json!({ "ok": true, "abi": ABI_VERSION, "data": data })
}

fn classify_failure(resp: &Value, plugin: &str, verb: &str) -> AbiError {
    let message = resp
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("plugin call failed without an error message")
        .to_string();

    if let Some(kind) = resp.get("kind").and_then(|v| v.as_str()) {
        return AbiError::new(AbiErrorKind::from_str(kind), plugin, verb, message);
    }

    let lowered = message.to_ascii_lowercase();
    let kind = if lowered.contains("unknown verb")
        || lowered.contains("verb not supported")
        || lowered.contains("unsupported verb")
    {
        AbiErrorKind::VerbNotSupported
    } else if lowered.contains("timed out") || lowered.contains("timeout") {
        AbiErrorKind::Timeout
    } else {
        AbiErrorKind::PluginError
    };
    AbiError::new(kind, plugin, verb, message)
}

pub fn parse_response(resp: &Value, plugin: &str, verb: &str) -> AbiResult {
    let obj = match resp.as_object() {
        Some(o) if !o.is_empty() => o,
        _ => {
            let unknown_name_hint = if KNOWN_PLUGINS.contains(&plugin) {
                String::new()
            } else {
                format!(" -- '{}' is not in this build's KNOWN_PLUGINS list ({:?}), so this is very likely a caller typo/wrong name rather than a genuinely unregistered plugin", plugin, KNOWN_PLUGINS)
            };
            return Err(AbiError::new(
                AbiErrorKind::PluginNotFound,
                plugin,
                verb,
                format!("no response from plugin '{}' (verb '{}'): the host returned no payload, which means the call was never routed -- the plugin is not registered or failed to load{}", plugin, verb, unknown_name_hint),
            ));
        }
    };

    let ok = obj.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !ok {
        return Err(classify_failure(resp, plugin, verb));
    }

    match obj.get("data") {
        Some(data) => Ok(data.clone()),
        None => {
            let mut passthrough = Map::new();
            for (k, v) in obj {
                if k != "ok" && k != "abi" {
                    passthrough.insert(k.clone(), v.clone());
                }
            }
            Ok(Value::Object(passthrough))
        }
    }
}

pub fn call(plugin: &str, verb: &str, body: &Value) -> AbiResult {
    let envelope = request_envelope(plugin, verb, body);
    let wire = match (body.as_object(), envelope.as_object()) {
        (Some(b), Some(e)) => {
            let mut merged = b.clone();
            for (k, v) in e {
                merged.insert(k.clone(), v.clone());
            }
            Value::Object(merged)
        }
        _ => envelope,
    };
    let resp = plugin_call(plugin, verb, &wire);
    parse_response(&resp, plugin, verb)
}

pub fn capabilities(plugin: &str) -> Result<Option<Capability>, AbiError> {
    match call(plugin, "capabilities", &json!({})) {
        Ok(data) => {
            let verbs = data
                .get("verbs")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            let abi = data.get("abi").and_then(|v| v.as_u64()).unwrap_or(ABI_VERSION);
            Ok(Some(Capability { plugin: plugin.to_string(), abi, verbs }))
        }
        Err(e) if e.kind == AbiErrorKind::VerbNotSupported => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn supports(plugin: &str, verb: &str) -> Result<Option<bool>, AbiError> {
    Ok(capabilities(plugin)?.map(|c| c.supports(verb)))
}

pub fn declare(plugin: &str, verbs: &[&str]) -> Capability {
    Capability {
        plugin: plugin.to_string(),
        abi: ABI_VERSION,
        verbs: verbs.iter().map(|v| v.to_string()).collect(),
    }
}
