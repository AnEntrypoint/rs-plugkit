#![cfg(target_arch = "wasm32")]
//! Versioned inter-plugin call ABI: the stable contract essential plugins are
//! cast against.
//!
//! Why this exists: `host_plugin_call` is a raw string-in/string-out pipe, and
//! every caller today re-invents the response convention on top of it. The
//! result is a real, load-bearing ambiguity -- `plugin_call` funnels through
//! `unpack_to_value`, which returns `Value::Null` whenever the host hands back
//! a null/empty pointer pair. Every consumer then reads that Null through the
//! same two lines (live, at `libsql_wasm.rs:9-15`, `verbs.rs:11-17`,
//! `code_index.rs:30-32`):
//!
//! ```ignore
//! let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
//! let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("<fallback>");
//! ```
//!
//! So a host that never routed the call at all (plugin absent -> Null) and a
//! plugin that really ran and really failed (`{ok:false,error:"..."}`) collapse
//! into the SAME observable: `ok=false` plus a caller-invented fallback string.
//! A caller cannot tell "libsql is not loaded" from "libsql rejected my SQL",
//! which are opposite problems with opposite fixes -- one is a deployment
//! fault, the other a bug in the request. That is exactly the confusion this
//! module removes, and it is the same class of defect already fixed once in
//! `verbs.rs` recall (an embedder-dead path that returned `ok:true` with zero
//! hits, indistinguishable from an honest empty result).
//!
//! # FROZEN -- these two shapes are the contract; changing them is a breaking change
//!
//! 1. **Request envelope.** Every call carries exactly these four keys at the
//!    top level, and the body is nested rather than flattened:
//!
//!    ```json
//!    { "abi": 1, "plugin": "libsql", "verb": "query", "body": { ... } }
//!    ```
//!
//!    `abi` is [`ABI_VERSION`], an integer that only ever increments. The body
//!    is NESTED, deliberately: flattening it would make any future envelope key
//!    collide with a plugin's own field name, which is unfixable after the fact.
//!
//! 2. **Response shape.** One consistent shape, both directions:
//!
//!    ```json
//!    { "ok": true,  "abi": 1, "data":  { ... } }
//!    { "ok": false, "abi": 1, "error": "...", "kind": "verb-not-supported" }
//!    ```
//!
//!    `ok` is always present and always a real bool. On failure `kind` is
//!    always one of the [`AbiErrorKind`] wire strings below -- never absent,
//!    never free-form. `error` stays human-readable and is never parsed for
//!    control flow; `kind` is the machine-readable half. That split is the
//!    whole point: today's callers string-match error text or give up.
//!
//! 3. **The error taxonomy itself** -- the four `kind` values and their
//!    meanings are frozen (see [`AbiErrorKind`]). New kinds may be ADDED; an
//!    existing one is never renamed, removed, or given a new meaning.
//!
//! # EXTENSIBLE -- these grow without breaking anyone
//!
//! * **New verbs.** A plugin may support any verb; nothing here enumerates a
//!   closed set. Capability declarations are data, not code.
//! * **New capabilities.** [`Capability`] carries an open `verbs` list, so a
//!   plugin advertising more verbs is purely additive.
//! * **New error kinds.** [`AbiErrorKind::Other`] is the forward-compatible
//!   escape hatch: an OLD build meeting a NEWER kind string parses it as
//!   `Other(s)` and keeps the original text, rather than misclassifying it as
//!   a known kind or hard-failing. This is what makes rolling upgrades safe in
//!   a process-wide plugin instance shared across concurrently-active projects.
//! * **Extra response fields.** Unknown keys are ignored, never rejected.
//!
//! # Compatibility with plugins that predate this envelope
//!
//! Non-negotiable, because `libsql` and `bert` are live and out-of-process:
//! this module CANNOT assume the far side speaks the envelope. [`parse_response`]
//! therefore accepts today's bare `{ok,error,rows,...}` shape too, and when
//! `ok:true` carries no `data` key it preserves the WHOLE response object as
//! the data payload -- so an enveloped caller still reads `rows`/`embedding`
//! off a legacy plugin. Without that, wrapping `libsql` would silently return
//! empty rows to every existing consumer.
//!
//! Nothing here changes an existing signature in `host_abi.rs`; this is a
//! strictly additive layer over the unmodified `plugin_call`.

use serde_json::{json, Map, Value};

use crate::wasm_dispatch::plugin_call;

/// Current envelope version. Incremented ONLY for a breaking change to the
/// frozen request/response shapes above -- never for an added verb, an added
/// capability, or an added error kind, all of which are backward-compatible by
/// construction.
pub const ABI_VERSION: u64 = 1;

/// Why a plugin call failed, as a closed set a caller can branch on.
///
/// Every plugin this crate ever calls by name, declared in one place instead
/// of scattered as a bare string literal at each `plugin_call("...", ...)`
/// call site (grepped: 19 call sites across code_index.rs, git_commit_vectors.rs,
/// libsql_wasm.rs, rssearch_vectors.rs, vecstore.rs, naming only "libsql",
/// "bert", "treesitter" -- "gm" itself is never a `plugin_call` target since
/// it is this crate). Not a full declarative manifest (no schema, no
/// discovery, no support for a plugin unknown at compile time) -- that is a
/// larger design decision this const deliberately does not attempt. What it
/// DOES give: a real, compile-time-anchored list `parse_response` can check a
/// requested name against, so a caller misspelling or inventing a plugin name
/// gets a distinct signal from "the correctly-named plugin genuinely isn't
/// registered on this host" instead of both looking identical. Sibling list:
/// agentplug's daemon.rs pre-warms `["gm", "libsql", "bert", "treesitter"]` at
/// boot -- that is a SEPARATE repo/crate with no shared-constants mechanism
/// between them, so keeping the two in sync is a manual discipline, not an
/// enforced one; a genuine cross-repo manifest schema would be needed to close
/// that gap for real.
pub const KNOWN_PLUGINS: &[&str] = &["libsql", "bert", "treesitter"];

/// The distinction that motivates the whole type: `PluginNotFound` and
/// `PluginError` are opposite faults. The first means the plugin never ran (a
/// deployment/wiring problem -- retrying the same call forever cannot help).
/// The second means it ran and rejected the request (a bug in the request --
/// retrying identically also cannot help, but the fix is in the caller). Today
/// both surface as `ok:false` with an invented message, so callers cannot tell
/// a missing plugin from a bad query, and neither can a human reading a log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiErrorKind {
    /// The host could not route the call: no plugin registered under that name.
    /// Detected via the Null return documented at the module level -- the host
    /// hands back an empty pointer pair, so there is no response object at all.
    PluginNotFound,
    /// The plugin exists and answered, but does not implement this verb. Callers
    /// use this to fall back to another verb or degrade a feature, rather than
    /// treating it as a hard failure.
    VerbNotSupported,
    /// The plugin ran the verb and it failed. `error` carries the plugin's own
    /// message -- this is the only kind where that text is authored by the
    /// plugin rather than by this layer.
    PluginError,
    /// The call exceeded a deadline. Distinct from `PluginError` because it is
    /// the one kind where retrying the identical call can legitimately succeed.
    Timeout,
    /// A kind emitted by a NEWER build than this one. Preserved verbatim so an
    /// old reader neither misclassifies it nor crashes; see the extensibility
    /// note above.
    Other(String),
}

impl AbiErrorKind {
    /// Frozen wire strings. These exact spellings are part of the contract --
    /// they appear in stored events and in other plugins' logs, so renaming one
    /// silently breaks every consumer that already matched on it.
    pub fn as_str(&self) -> &str {
        match self {
            AbiErrorKind::PluginNotFound => "plugin-not-found",
            AbiErrorKind::VerbNotSupported => "verb-not-supported",
            AbiErrorKind::PluginError => "plugin-error",
            AbiErrorKind::Timeout => "timeout",
            AbiErrorKind::Other(s) => s.as_str(),
        }
    }

    /// Parse a wire string. An unrecognized value becomes `Other` rather than a
    /// default known kind: guessing here would actively mislead a caller into
    /// branching on a fault that did not happen.
    pub fn from_str(s: &str) -> AbiErrorKind {
        match s {
            "plugin-not-found" => AbiErrorKind::PluginNotFound,
            "verb-not-supported" => AbiErrorKind::VerbNotSupported,
            "plugin-error" => AbiErrorKind::PluginError,
            "timeout" => AbiErrorKind::Timeout,
            other => AbiErrorKind::Other(other.to_string()),
        }
    }

    /// Whether retrying the identical call could plausibly succeed. Only
    /// `Timeout` qualifies -- a missing plugin stays missing, and an
    /// unsupported verb or a rejected request is deterministic. Callers use
    /// this instead of re-deriving retry policy per call site.
    pub fn is_retryable(&self) -> bool {
        matches!(self, AbiErrorKind::Timeout)
    }
}

/// A failed call: the machine-readable `kind` plus the human-readable message.
#[derive(Debug, Clone)]
pub struct AbiError {
    pub kind: AbiErrorKind,
    pub message: String,
    /// Which plugin/verb produced this, so an error crossing several layers
    /// stays attributable without every layer re-wrapping the message.
    pub plugin: String,
    pub verb: String,
}

impl AbiError {
    pub fn new(kind: AbiErrorKind, plugin: &str, verb: &str, message: impl Into<String>) -> AbiError {
        AbiError { kind, message: message.into(), plugin: plugin.to_string(), verb: verb.to_string() }
    }

    /// Render in the FROZEN failure shape, for a plugin answering a call.
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

/// Result of an enveloped call. `Ok` carries only the data payload, because a
/// caller that already matched `Ok` has no reason to re-check an `ok` field --
/// the type makes the check unforgettable, which the JSON shape alone cannot.
pub type AbiResult = Result<Value, AbiError>;

/// What a plugin advertises it can do.
///
/// `verbs` is intentionally an open list rather than an enum: the whole point
/// of a capability declaration is that a caller can discover support WITHOUT
/// this crate having to know every verb in advance.
#[derive(Debug, Clone)]
pub struct Capability {
    pub plugin: String,
    pub abi: u64,
    pub verbs: Vec<String>,
}

impl Capability {
    /// Whether this plugin declares support for `verb`.
    pub fn supports(&self, verb: &str) -> bool {
        self.verbs.iter().any(|v| v == verb)
    }

    /// Whether this crate can talk to the declared ABI. A plugin on a NEWER
    /// major envelope may have changed a frozen shape, so treating it as
    /// compatible would be unsound; an older one is fine because the frozen
    /// shapes only ever gain optional fields.
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

/// Build the FROZEN request envelope. Separated from [`call`] so a caller that
/// needs the raw envelope (to log it, or to hand it to a different transport)
/// gets the identical bytes the real call would send.
pub fn request_envelope(plugin: &str, verb: &str, body: &Value) -> Value {
    json!({ "abi": ABI_VERSION, "plugin": plugin, "verb": verb, "body": body })
}

/// Render a successful response in the FROZEN shape.
pub fn ok_response(data: Value) -> Value {
    json!({ "ok": true, "abi": ABI_VERSION, "data": data })
}

/// Classify a response object that has already been established as a failure.
///
/// Order matters. An explicit `kind` from the far side always wins, because the
/// plugin knows its own fault better than any inference here. Only when `kind`
/// is absent -- i.e. a pre-envelope plugin -- does this fall back to a narrow,
/// deliberately conservative text sniff. That sniff is anchored on phrases the
/// host itself emits (`verbs.rs:1621` answers an unrecognized verb with the
/// literal "unknown verb", and `verbs.rs:1599` with "verb not supported"), so
/// it recognizes real strings this codebase actually produces rather than
/// guessing. Anything unrecognized stays `PluginError`: misreporting a genuine
/// plugin failure as "verb not supported" would send a caller down a fallback
/// path for a fault that fallback cannot fix.
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

/// Parse any plugin response -- enveloped or legacy -- into an [`AbiResult`].
///
/// The legacy accommodation is load-bearing, not politeness: `libsql` and
/// `bert` are live out-of-process plugins that answer with a bare
/// `{ok:true, rows:[...]}` / `{ok:true, embedding:[...]}` and know nothing
/// about this envelope. When `ok:true` carries no `data` key, the WHOLE
/// response object becomes the payload, so `rows`/`embedding` survive. Doing
/// the obvious thing instead (defaulting missing `data` to `Null`) would hand
/// every existing consumer an empty result while still reporting success --
/// reintroducing, at this very layer, the exact silent-empty-success bug this
/// module exists to eliminate.
pub fn parse_response(resp: &Value, plugin: &str, verb: &str) -> AbiResult {
    // A Null/non-object response means the host never produced a response
    // object at all -- the empty pointer pair `unpack_to_value` turns into
    // Null. This is the ONLY available signal separating "no such plugin" from
    // "plugin ran and failed", and it is why this case is checked first.
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
        // Legacy success: preserve the whole object (minus the envelope's own
        // bookkeeping keys, which are never part of a plugin's payload).
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

/// The typed wrapper: apply the envelope, make the call, parse the response.
///
/// This is the only function most callers need. It never returns a bare `Value`
/// on failure, so the ambiguity described at the module level cannot survive
/// past this boundary.
pub fn call(plugin: &str, verb: &str, body: &Value) -> AbiResult {
    let envelope = request_envelope(plugin, verb, body);
    // The far side may be a pre-envelope plugin that reads the body's fields at
    // the TOP level (every live `libsql`/`bert` call does exactly that). Sending
    // only the nested envelope would break all of them, so the envelope keys are
    // merged ALONGSIDE the original body: new plugins read `body`, old plugins
    // read their own fields, and neither needs to know about the other. The
    // envelope keys are added second so a plugin body can never shadow them.
    let wire = match (body.as_object(), envelope.as_object()) {
        (Some(b), Some(e)) => {
            let mut merged = b.clone();
            for (k, v) in e {
                merged.insert(k.clone(), v.clone());
            }
            Value::Object(merged)
        }
        // A non-object body has no fields to preserve, so the envelope alone is
        // already complete.
        _ => envelope,
    };
    let resp = plugin_call(plugin, verb, &wire);
    parse_response(&resp, plugin, verb)
}

/// Ask a plugin what it supports, via the conventional `capabilities` verb.
///
/// A plugin that does not implement the verb is NOT an error in itself -- it is
/// simply undeclared, which is why the `VerbNotSupported` case maps to
/// `Ok(None)`. Collapsing that into `Err` would make every pre-envelope plugin
/// look broken. A genuinely missing plugin still surfaces as `Err`, because
/// that is a real fault a caller must see.
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

/// Check support BEFORE calling, so a caller can pick a verb rather than
/// discovering the gap through a failed call.
///
/// `None` means "cannot tell" -- the plugin declares no capabilities -- and is
/// deliberately distinct from `Some(false)` ("declared, and this verb is not in
/// the list"). A caller must not read undeclared as unsupported: every live
/// pre-envelope plugin is undeclared yet fully functional.
pub fn supports(plugin: &str, verb: &str) -> Result<Option<bool>, AbiError> {
    Ok(capabilities(plugin)?.map(|c| c.supports(verb)))
}

/// Build this plugin's own capability declaration, for answering `capabilities`.
pub fn declare(plugin: &str, verbs: &[&str]) -> Capability {
    Capability {
        plugin: plugin.to_string(),
        abi: ABI_VERSION,
        verbs: verbs.iter().map(|v| v.to_string()).collect(),
    }
}
