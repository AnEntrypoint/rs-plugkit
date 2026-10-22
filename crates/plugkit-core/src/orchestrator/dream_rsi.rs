#[cfg(target_arch = "wasm32")]
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone)]
struct Node {
    id: String,
    score: f64,
    cost: u64,
    round: u64,
    children: Vec<String>,
}

#[derive(Clone)]
struct Policy {
    id: String,
    roots: Vec<String>,
    max_nodes: usize,
    attempt: Option<u64>,
    group_size: Option<u64>,
    stop: Option<Stop>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Stop {
    Budget,
    Converged,
    Exhausted,
}

impl Stop {
    const NAMES: [(&'static str, Stop); 3] = [
        ("budget", Stop::Budget),
        ("converged", Stop::Converged),
        ("exhausted", Stop::Exhausted),
    ];

    fn parse(raw: &str) -> Option<Stop> {
        Self::NAMES
            .iter()
            .find(|(name, _)| *name == raw)
            .map(|(_, stop)| *stop)
    }

    fn name(self) -> &'static str {
        Self::NAMES
            .iter()
            .find(|(_, stop)| *stop == self)
            .map_or("budget", |(name, _)| name)
    }
}

struct World {
    id: String,
    nodes: BTreeMap<String, Node>,
}

pub const INSUFFICIENT_RECORD_THRESHOLD: u64 = 20;

fn string_field(value: &Value, field: &str) -> Result<String, String> {
    let value = value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("dream-replay-cycle requires non-empty {field}"))?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(format!(
            "dream-replay-cycle {field} must be an opaque ASCII identifier"
        ));
    }
    Ok(value.to_string())
}

fn array_field<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>, String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("dream-replay-cycle requires array {field}"))
}

fn parse_policy(value: &Value) -> Result<Policy, String> {
    let id = string_field(value, "id")?;
    let roots = array_field(value, "roots")?
        .iter()
        .map(|root| {
            root.as_str()
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("dream-replay-cycle policy {id} has an empty root"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if roots.is_empty() {
        return Err(format!(
            "dream-replay-cycle policy {id} requires at least one root"
        ));
    }
    let max_nodes = value
        .get("max_nodes")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("dream-replay-cycle policy {id} requires positive safe max_nodes"))?;
    let group_size = match value.get("group_size") {
        None | Some(Value::Null) => None,
        Some(raw) => Some(
            raw.as_u64()
                .filter(|value| *value > 0)
                .ok_or_else(|| format!("dream-replay-cycle policy {id} field group_size must be a positive integer when present"))?,
        ),
    };
    let stop = match value.get("stop") {
        None | Some(Value::Null) => None,
        Some(raw) => Some(
            raw.as_str()
                .and_then(Stop::parse)
                .ok_or_else(|| {
                    let names = Stop::NAMES.map(|(name, _)| name);
                    format!("dream-replay-cycle policy {id} field stop must be one of {names:?} when present")
                })?,
        ),
    };
    Ok(Policy {
        id,
        roots,
        max_nodes,
        attempt: value.get("attempt").and_then(Value::as_u64),
        group_size,
        stop,
    })
}

fn evaluate_policy(
    world: &World,
    policy: &Policy,
    beta1: f64,
    beta2: f64,
) -> Result<Value, String> {
    let (budget, budget_source) = match policy.stop {
        Some(Stop::Exhausted) => (usize::MAX, "stop-exhausted"),
        _ => (policy.max_nodes, "max_nodes"),
    };
    let wave_size = policy.group_size.map_or(1, |size| size as usize).max(1);
    let attempts_per_node = policy.attempt.unwrap_or(1).max(1);
    let mut queue = VecDeque::from(policy.roots.clone());
    let mut observed = BTreeSet::new();
    let mut quality = f64::NEG_INFINITY;
    let mut cost = 0_u64;
    let mut retries = 0_u64;
    let mut recorded_rounds = BTreeSet::new();
    let mut waves = 0_usize;
    let mut converged = false;
    while !queue.is_empty() && observed.len() < budget && !converged {
        let quality_before_wave = quality;
        let mut expanded_in_wave = 0_usize;
        while expanded_in_wave < wave_size && observed.len() < budget {
            let Some(id) = queue.pop_front() else { break };
            if !observed.insert(id.clone()) {
                continue;
            }
            let node = world.nodes.get(&id).ok_or_else(|| {
                format!(
                    "dream-replay-cycle policy {} asks world {} for unobserved node {id}",
                    policy.id, world.id
                )
            })?;
            expanded_in_wave += 1;
            quality = quality.max(node.score);
            let node_attempts = if node.score < 1.0 { attempts_per_node } else { 1 };
            retries += node_attempts - 1;
            cost = node
                .cost
                .checked_mul(node_attempts)
                .and_then(|charged| cost.checked_add(charged))
                .ok_or_else(|| "dream-replay-cycle cost overflow".to_string())?;
            recorded_rounds.insert(node.round);
            for child in &node.children {
                if !observed.contains(child) {
                    queue.push_back(child.clone());
                }
            }
        }
        if expanded_in_wave > 0 {
            waves += 1;
        }
        converged = policy.stop == Some(Stop::Converged)
            && waves > 1
            && quality <= quality_before_wave;
    }
    if observed.is_empty() {
        return Err(format!(
            "dream-replay-cycle policy {} observed no nodes in world {}",
            policy.id, world.id
        ));
    }
    let rounds = if policy.group_size.is_some() { waves } else { recorded_rounds.len() };
    let parallelism_bonus = observed.len() as f64 / rounds.max(1) as f64;
    let replay_score = quality - beta1 * cost as f64 + beta2 * parallelism_bonus;
    if !replay_score.is_finite() {
        return Err(format!(
            "dream-replay-cycle policy {} replay score in world {} is non-finite",
            policy.id, world.id
        ));
    }
    Ok(json!({
        "world_id": world.id,
        "observed_node_ids": observed.into_iter().collect::<Vec<_>>(),
        "quality": quality,
        "cost": cost,
        "retries": retries,
        "rounds": rounds,
        "converged": converged,
        "budget_source": budget_source,
        "parallelism_bonus": parallelism_bonus,
        "replay_score": replay_score,
    }))
}

const PROMOTION_MARGIN: f64 = 0.02;
const PROMOTION_MIN_WORLDS: usize = 3;
const PROMOTION_MIN_WIN_FRACTION: f64 = 0.5;

fn evaluate_worlds(
    baseline_id: String,
    mut policies: Vec<Policy>,
    worlds: Vec<World>,
    beta1: f64,
    beta2: f64,
) -> Result<Value, String> {
    let mut policy_ids = BTreeSet::new();
    for policy in &policies {
        if !policy_ids.insert(policy.id.clone()) {
            return Err(format!("dream-replay-cycle repeats policy {}", policy.id));
        }
    }
    if !policy_ids.contains(&baseline_id) {
        return Err(format!(
            "dream-replay-cycle baseline_policy_id {baseline_id} is absent from policies"
        ));
    }
    if worlds.is_empty() {
        return Err("dream-replay-cycle requires at least one sealed world".to_string());
    }
    if let Some(position) = policies.iter().position(|policy| policy.id == baseline_id) {
        let incumbent = policies.remove(position);
        policies.insert(0, incumbent);
    }
    // Replay every world under every policy once. A world that fails under any policy is dropped from
    // every ranking so the candidates stay comparable, and it is reported in skipped_worlds.
    let evaluated: Vec<Vec<Result<Value, String>>> = policies
        .iter()
        .map(|policy| worlds.iter().map(|world| evaluate_policy(world, policy, beta1, beta2)).collect())
        .collect();
    let mut usable = vec![true; worlds.len()];
    let mut skipped_worlds = Vec::new();
    for (policy_index, policy) in policies.iter().enumerate() {
        for (index, result) in evaluated[policy_index].iter().enumerate() {
            if let Err(error) = result {
                if usable[index] {
                    usable[index] = false;
                    skipped_worlds.push(json!({ "world_id": worlds[index].id, "policy_id": policy.id, "error": error }));
                }
            }
        }
    }
    let usable_count = usable.iter().filter(|kept| **kept).count();
    if usable_count == 0 {
        return Err("dream-replay-cycle: no sealed world replays under every policy".to_string());
    }
    let mut rankings = Vec::new();
    for (policy_index, policy) in policies.iter().enumerate() {
        let replays = evaluated[policy_index]
            .iter()
            .zip(usable.iter())
            .filter(|(_, kept)| **kept)
            .map(|(result, _)| result.clone())
            .collect::<Result<Vec<_>, _>>()?;
        let scores = replays
            .iter()
            .map(|replay| {
                replay
                    .get("replay_score")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| {
                        format!("dream-replay-cycle policy {} replay lacks a numeric replay_score", policy.id)
                    })
            })
            .collect::<Result<Vec<f64>, String>>()?;
        let score = scores.iter().sum::<f64>() / scores.len() as f64;
        if !score.is_finite() && policy.id == baseline_id {
            return Err(format!("dream-replay-cycle policy {} score is non-finite", policy.id));
        }
        let cost = replays
            .iter()
            .map(|replay| {
                replay.get("cost").and_then(Value::as_u64).ok_or_else(|| {
                    format!("dream-replay-cycle policy {} replay lacks a numeric cost", policy.id)
                })
            })
            .sum::<Result<u64, String>>()?;
        rankings.push(json!({
            "policy_id": policy.id,
            "score": score,
            "cost": cost,
            "attempt": policy.attempt.unwrap_or(1).max(1),
            "group_size": policy.group_size.unwrap_or(1).max(1),
            "stop": policy.stop.map(Stop::name),
            "replays": replays,
        }));
    }
    let baseline = rankings
        .iter()
        .find(|ranking| {
            ranking.get("policy_id").and_then(Value::as_str) == Some(baseline_id.as_str())
        })
        .cloned()
        .ok_or_else(|| "dream-replay-cycle baseline ranking missing".to_string())?;
    let baseline_score = baseline
        .get("score")
        .and_then(Value::as_f64)
        .ok_or_else(|| "dream-replay-cycle baseline score missing".to_string())?;
    let replay_score = |replay: &Value| replay.get("replay_score").and_then(Value::as_f64).unwrap_or(f64::NEG_INFINITY);
    let baseline_replays = baseline.get("replays").and_then(Value::as_array).cloned().unwrap_or_default();
    for ranking in rankings.iter_mut() {
        let wins = ranking
            .get("replays")
            .and_then(Value::as_array)
            .map_or(0, |replays| {
                replays
                    .iter()
                    .zip(baseline_replays.iter())
                    .filter(|(challenger, incumbent)| replay_score(challenger) > replay_score(incumbent))
                    .count()
            });
        let win_fraction = if usable_count == 0 { 0.0 } else { wins as f64 / usable_count as f64 };
        ranking["win_fraction"] = json!(win_fraction);
    }
    let clears_margin = |ranking: &Value| -> bool {
        ranking.get("score").and_then(Value::as_f64).unwrap_or(f64::NEG_INFINITY) > baseline_score + PROMOTION_MARGIN
    };
    let clears_win_fraction = |ranking: &Value| -> bool {
        ranking.get("win_fraction").and_then(Value::as_f64).unwrap_or(0.0) >= PROMOTION_MIN_WIN_FRACTION
    };
    let margin_only = rankings.iter().any(|ranking| clears_margin(ranking) && !clears_win_fraction(ranking));
    let selected = rankings
        .iter()
        .filter(|ranking| usable_count >= PROMOTION_MIN_WORLDS && clears_margin(ranking) && clears_win_fraction(ranking))
        .max_by(|left, right| {
            let score = |ranking: &Value| {
                ranking
                    .get("score")
                    .and_then(Value::as_f64)
                    .unwrap_or(f64::NEG_INFINITY)
            };
            score(left)
                .partial_cmp(&score(right))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    right
                        .get("cost")
                        .and_then(Value::as_u64)
                        .cmp(&left.get("cost").and_then(Value::as_u64))
                })
                .then_with(|| {
                    right
                        .get("policy_id")
                        .and_then(Value::as_str)
                        .cmp(&left.get("policy_id").and_then(Value::as_str))
                })
        })
        .cloned()
        .unwrap_or(baseline.clone());
    let declared = |key: &str| selected.get(key).cloned().unwrap_or(Value::Null);
    let selected_score = selected.get("score").and_then(Value::as_f64);
    let improved = selected.get("policy_id") != baseline.get("policy_id")
        && selected_score.unwrap_or(f64::NEG_INFINITY) > baseline_score;
    Ok(json!({
        "ok": true,
        "baseline_policy_id": baseline_id,
        "selected_policy_id": selected.get("policy_id").and_then(Value::as_str),
        "baseline_score": baseline_score,
        "selected_score": selected_score,
        "attempt": declared("attempt"),
        "group_size": declared("group_size"),
        "stop": declared("stop"),
        "improved": improved,
        "promotion_reason": if improved {
            "improved"
        } else if usable_count < PROMOTION_MIN_WORLDS {
            "insufficient_worlds"
        } else if margin_only {
            "below_win_fraction"
        } else {
            "below_margin"
        },
        "promotion_margin": PROMOTION_MARGIN,
        "promotion_min_win_fraction": PROMOTION_MIN_WIN_FRACTION,
        "n_observations": usable_count,
        "skipped_worlds": skipped_worlds,
        "domain": if usable_count < PROMOTION_MIN_WORLDS { "disorder" } else { "order" },
        "beta1": beta1,
        "beta2": beta2,
        "rankings": rankings,
    }))
}

#[cfg(target_arch = "wasm32")]
fn session_store_path(session: &str, file: &str) -> String {
    let dir = format!(".gm/dream-rsi/{session}");
    if file.is_empty() {
        dir
    } else {
        format!("{dir}/{file}")
    }
}

#[cfg(target_arch = "wasm32")]
fn current_session_store_path(name: &str) -> Result<String, String> {
    Ok(session_store_path(&session_id()?, &format!("{name}.json")))
}

#[cfg(target_arch = "wasm32")]
fn is_gate_drift_code(code: &str) -> bool {
    matches!(
        code,
        crate::wasm_dispatch::ERR_CODE_GATE_DENIED
            | crate::wasm_dispatch::ERR_CODE_UNKNOWN_VERB
            | crate::wasm_dispatch::ERR_CODE_RETIRED_VERB
    )
}

#[cfg(target_arch = "wasm32")]
const FAILURE_CLASS_KEYS: [&str; 4] = ["error_code", "error_kind", "reason", "code"];

#[cfg(target_arch = "wasm32")]
const FAILURE_CLASS_MAX_CHARS: usize = 64;

#[cfg(target_arch = "wasm32")]
const MATCH_COUNT_KEYS: [&str; 3] = ["match_count", "total_matches", "returned"];

#[cfg(target_arch = "wasm32")]
fn token_like(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= FAILURE_CLASS_MAX_CHARS
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

#[cfg(target_arch = "wasm32")]
fn reply_layers(result: &Value) -> [Option<&Value>; 2] {
    [Some(result), result.get("data")]
}

#[cfg(target_arch = "wasm32")]
fn reply_field(result: &Value, key: &str) -> Option<String> {
    reply_layers(result).iter().flatten().find_map(|layer| {
        layer.get(key).and_then(Value::as_str).map(str::trim).filter(|text| token_like(text)).map(str::to_owned)
    })
}

#[cfg(target_arch = "wasm32")]
const ERROR_LINE_KEYS: [&str; 5] = ["stderr", "error", "message", "reason", "output"];

#[cfg(target_arch = "wasm32")]
const ERROR_LINE_MAX_CHARS: usize = 240;

#[cfg(target_arch = "wasm32")]
fn error_line(result: &Value, exit_code: i64) -> Option<String> {
    if exit_code == 0 {
        return None;
    }
    reply_layers(result).iter().flatten().find_map(|layer| {
        ERROR_LINE_KEYS.iter().find_map(|key| {
            layer
                .get(*key)
                .and_then(Value::as_str)
                .and_then(|text| text.lines().map(str::trim).find(|line| !line.is_empty()))
                .map(|line| line.chars().take(ERROR_LINE_MAX_CHARS).collect::<String>())
        })
    })
}

#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone, PartialEq)]
pub enum FailureClass {
    Ok,
    Coded(String),
    NoMatch,
    Unclassified,
}

#[cfg(target_arch = "wasm32")]
impl FailureClass {
    fn error_code(&self) -> Option<&str> {
        match self {
            Self::Coded(code) => Some(code),
            _ => None,
        }
    }

    fn no_match(&self) -> bool {
        matches!(self, Self::NoMatch)
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Coded(_) => "coded",
            Self::NoMatch => "no_match",
            Self::Unclassified => "unclassified",
        }
    }

    fn for_exit(self, exit_code: i64) -> Self {
        match (exit_code == 0, self) {
            (true, _) => Self::Ok,
            (false, Self::Ok) => Self::Unclassified,
            (false, other) => other,
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn failure_class(result: &Value, exit_code: i64) -> FailureClass {
    if exit_code == 0 {
        return FailureClass::Ok;
    }
    let layers = reply_layers(result);
    let error_code = layers.iter().flatten().find_map(|layer| {
        FAILURE_CLASS_KEYS.iter().find_map(|key| {
            layer.get(*key).and_then(Value::as_str).map(str::trim).filter(|text| token_like(text)).map(str::to_owned)
        })
    });
    if let Some(code) = error_code {
        return if code == "not_found" {
            FailureClass::NoMatch
        } else {
            FailureClass::Coded(code)
        };
    }
    let zero_matches = layers.iter().flatten().any(|layer| {
        MATCH_COUNT_KEYS.iter().any(|key| layer.get(*key).and_then(Value::as_u64) == Some(0))
    });
    if zero_matches {
        FailureClass::NoMatch
    } else {
        FailureClass::Coded("exit_nonzero".to_owned())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg(target_arch = "wasm32")]
struct ObservationRecord {
    dispatch_id: Option<String>,
    verb: Option<String>,
    fingerprint: Option<String>,
    exit_code: Option<i64>,
    gate_drift: Option<bool>,
    gate_id: Option<String>,
    predicate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prd_open_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mutable_open_count: Option<u64>,
    quality: Option<f64>,
    ts: Option<i64>,
    reply_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lean_node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    no_match: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_error_line: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    run_end: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    policy_id: Option<String>,
}

#[cfg(target_arch = "wasm32")]
impl ObservationRecord {
    fn from_value(value: &Value) -> Option<Self> {
        let record: Self = serde_json::from_value(value.clone()).ok()?;
        (record.exit_code.is_some() && record.is_consistent()).then_some(record)
    }

    fn is_consistent(&self) -> bool {
        let coded = self.error_code.is_some();
        let exit_ok = self.exit_code == Some(0);
        let outcome_ok = if exit_ok {
            !coded && !self.no_match
        } else {
            !(coded && self.no_match)
        };
        let expected_class = if coded {
            "coded"
        } else if self.no_match {
            "no_match"
        } else if exit_ok {
            "ok"
        } else {
            "unclassified"
        };
        outcome_ok && self.failure_class.as_deref().is_none_or(|name| name == expected_class)
    }

    fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }

    fn is_gate_drift_failure(&self) -> bool {
        !self.succeeded() && self.gate_drift.unwrap_or(false)
    }
}

#[cfg(target_arch = "wasm32")]
fn read_observation_values(raw: &str) -> (Vec<Value>, usize) {
    if raw.trim_start().starts_with('[') {
        return match serde_json::from_str::<Vec<Value>>(raw) {
            Ok(values) => (values, 0),
            Err(_) => (Vec::new(), 1),
        };
    }
    let mut rejected = 0;
    let values = raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| match serde_json::from_str::<Value>(line) {
            Ok(value) => Some(value),
            Err(_) => {
                rejected += 1;
                None
            }
        })
        .collect();
    (values, rejected)
}

#[cfg(target_arch = "wasm32")]
fn read_observations(raw: &str) -> (Vec<ObservationRecord>, usize) {
    let (values, parse_rejected) = read_observation_values(raw);
    let records: Vec<ObservationRecord> = values.iter().filter_map(ObservationRecord::from_value).collect();
    let shape_rejected = values.len() - records.len();
    (records, parse_rejected + shape_rejected)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn reply_sha256_hex(reply: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(reply.as_bytes()))
}

#[cfg(target_arch = "wasm32")]
pub struct ObservedDispatch<'a> {
    pub session_id: &'a str,
    pub dispatch_id: &'a str,
    pub verb: &'a str,
    pub fingerprint: &'a str,
    pub exit_code: i64,
    pub output_bytes: Option<u64>,
    pub gate_drift: bool,
    pub gate_id: Option<&'a str>,
    pub predicate: Option<&'a str>,
    pub first_error_line: Option<&'a str>,
    pub reply: &'a str,
    pub lean_node: Option<&'a str>,
    pub failure: FailureClass,
    pub run_end: bool,
}

#[cfg(target_arch = "wasm32")]
const OBSERVATIONS_LIVE_CAP: usize = 256;

#[cfg(target_arch = "wasm32")]
const OBSERVATIONS_ARCHIVE_MAX_CHUNKS: usize = 32;

#[cfg(target_arch = "wasm32")]
const OBSERVATIONS_ARCHIVE_PREFIX: &str = "observations-archive-";

#[cfg(target_arch = "wasm32")]
const OBSERVATIONS_LIVE_COUNT_FILE: &str = "observations.count";

#[cfg(target_arch = "wasm32")]
fn dir_entry_names(dir: &str) -> Vec<String> {
    let Some(Value::Array(entries)) = crate::pkfs::readdir(dir) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            entry
                .as_str()
                .or_else(|| entry.get("name").and_then(Value::as_str))
                .map(str::to_owned)
        })
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn archive_names(session_id: &str) -> Vec<String> {
    let mut names: Vec<(i64, String)> = dir_entry_names(&session_store_path(session_id, ""))
        .into_iter()
        .filter_map(|name| {
            let ts = name
                .strip_prefix(OBSERVATIONS_ARCHIVE_PREFIX)?
                .strip_suffix(".json")?
                .parse::<i64>()
                .ok()?;
            Some((ts, name))
        })
        .collect();
    names.sort_by_key(|(ts, _)| *ts);
    names.into_iter().map(|(_, name)| name).collect()
}

#[cfg(target_arch = "wasm32")]
fn archive_observations(session_id: &str, ts: i64, raw: &str) -> Option<String> {
    let path = session_store_path(session_id, &format!("{OBSERVATIONS_ARCHIVE_PREFIX}{ts}.json"));
    if crate::pkfs::exists(&path) || !crate::pkfs::write(&path, raw) {
        return None;
    }
    let names = archive_names(session_id);
    for stale in names.iter().take(names.len().saturating_sub(OBSERVATIONS_ARCHIVE_MAX_CHUNKS)) {
        crate::pkfs::remove_file(&session_store_path(session_id, stale));
    }
    Some(path)
}

#[cfg(target_arch = "wasm32")]
fn live_count_path(session_id: &str) -> String {
    session_store_path(session_id, OBSERVATIONS_LIVE_COUNT_FILE)
}

#[cfg(target_arch = "wasm32")]
fn read_live_count(session_id: &str) -> Option<usize> {
    crate::pkfs::read_to_string(&live_count_path(session_id))?.trim().parse().ok()
}

#[cfg(target_arch = "wasm32")]
fn write_live_count(session_id: &str, count: usize) {
    crate::pkfs::write(&live_count_path(session_id), &format!("{count}\n"));
}

#[cfg(target_arch = "wasm32")]
fn bump_live_count(session_id: &str) {
    const LIVE_COUNT_CAS_ATTEMPTS: usize = 64;
    let path = live_count_path(session_id);
    for _ in 0..LIVE_COUNT_CAS_ATTEMPTS {
        let Some(raw) = crate::pkfs::read_to_string(&path) else {
            return;
        };
        let Ok(count) = raw.trim().parse::<usize>() else {
            return;
        };
        if matches!(
            crate::pkfs::cas_write(&path, &raw, &format!("{}\n", count + 1)),
            crate::pkfs::CasWriteOutcome::Swapped
        ) {
            return;
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn recover_observations_tail(path: &str) -> Option<String> {
    let Some(raw) = crate::pkfs::read_to_string(path) else {
        return Some(String::new());
    };
    if raw.is_empty() || raw.ends_with('\n') {
        return Some(raw);
    }
    let keep = raw.rfind('\n').map_or(0, |i| i + 1);
    let tail = &raw[keep..];
    let tail_parses = serde_json::from_str::<Value>(tail.trim()).is_ok();
    if !tail_parses && !crate::pkfs::append(&format!("{path}.torn"), &format!("{tail}\n")) {
        return None;
    }
    let body = if tail_parses {
        format!("{raw}\n")
    } else {
        raw[..keep].to_owned()
    };
    match crate::pkfs::cas_write(path, &raw, &body) {
        crate::pkfs::CasWriteOutcome::Swapped => Some(body),
        _ => None,
    }
}

#[cfg(target_arch = "wasm32")]
fn rotate_observations_if_full(session_id: &str, path: &str, ts: i64) {
    if read_live_count(session_id).is_some_and(|count| count < OBSERVATIONS_LIVE_CAP) {
        return;
    }
    let Some(raw) = crate::pkfs::read_to_string(path) else {
        return;
    };
    if raw.trim().is_empty() {
        write_live_count(session_id, 0);
        return;
    }
    let legacy_array = raw.trim_start().starts_with('[');
    let live_lines = raw.lines().filter(|line| !line.trim().is_empty()).count();
    if !legacy_array && live_lines < OBSERVATIONS_LIVE_CAP {
        write_live_count(session_id, live_lines);
        return;
    }
    let Some(archive) = archive_observations(session_id, ts, &raw) else {
        return;
    };
    let swapped = matches!(
        crate::pkfs::cas_write(path, &raw, ""),
        crate::pkfs::CasWriteOutcome::Swapped
    );
    if !swapped {
        crate::pkfs::remove_file(&archive);
        write_live_count(session_id, OBSERVATIONS_LIVE_CAP);
        return;
    }
    write_live_count(session_id, 0);
}

#[cfg(target_arch = "wasm32")]
fn session_history(session_id: &str) -> (Vec<ObservationRecord>, usize) {
    let mut records: Vec<ObservationRecord> = Vec::new();
    let mut rejected = 0;
    let mut absorb = |raw: &str| {
        let (found, dropped) = read_observations(raw);
        records.extend(found);
        rejected += dropped;
    };
    for name in archive_names(session_id) {
        if let Some(raw) = crate::pkfs::read_to_string(&session_store_path(session_id, &name)) {
            absorb(&raw);
        }
    }
    if let Some(raw) = recover_observations_tail(&session_store_path(session_id, OBSERVATIONS_FILE)) {
        absorb(&raw);
    }
    (records, rejected)
}

#[cfg(target_arch = "wasm32")]
#[derive(Default)]
struct LedgerCheck {
    entries: std::collections::HashMap<String, Value>,
    unreadable: Option<&'static str>,
    evicted_possible: bool,
    newest_ts: u64,
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy, PartialEq)]
enum Confirmation {
    Ledger,
    Retained,
}

#[cfg(target_arch = "wasm32")]
impl LedgerCheck {
    fn load() -> Self {
        let Some(cwd) = crate::wasm_dispatch::host_cwd_string().filter(|cwd| !cwd.is_empty()) else {
            return LedgerCheck { unreadable: Some("cwd_unavailable"), ..Default::default() };
        };
        let path = crate::dispatch_ledger::ledger_path(&cwd);
        let Some(raw) = crate::pkfs::read_to_string(&path) else {
            if crate::pkfs::exists(&path) {
                return LedgerCheck { unreadable: Some("ledger_unreadable"), ..Default::default() };
            }
            return LedgerCheck::default();
        };
        let Ok(parsed) = serde_json::from_str::<Vec<Value>>(&raw) else {
            return LedgerCheck { unreadable: Some("ledger_unparsed"), ..Default::default() };
        };
        let evicted_possible = parsed.len() >= crate::dispatch_ledger::RETAINED_ENTRIES;
        let newest_ts = parsed.iter().filter_map(|entry| entry.get("ts")?.as_u64()).max().unwrap_or(0);
        let entries = parsed
            .into_iter()
            .filter_map(|entry| Some((entry.get("dispatch_id")?.as_str()?.to_owned(), entry)))
            .collect();
        LedgerCheck { entries, evicted_possible, newest_ts, ..Default::default() }
    }

    fn verdict(&mut self, session_id: &str, record: &ObservationRecord) -> Result<Confirmation, &'static str> {
        let dispatch_id = record.dispatch_id.as_deref().ok_or("no_dispatch_id")?;
        let Some(entry) = self.entries.get(dispatch_id) else {
            let aged_out = record.ts.is_some_and(|ts| u64::try_from(ts).is_ok_and(|ts| ts.saturating_add(crate::dispatch_ledger::RETAIN_MS) < self.newest_ts));
            if (self.evicted_possible || aged_out) && self.unreadable.is_none() {
                return Ok(Confirmation::Retained);
            }
            return Err(self.unreadable.unwrap_or("unconfirmed_by_ledger"));
        };
        if entry.get("session_id").and_then(Value::as_str) != Some(session_id) {
            return Err("ledger_session_mismatch");
        }
        if entry.get("exit_code").and_then(Value::as_i64) != record.exit_code {
            return Err("ledger_exit_mismatch");
        }
        if entry.get("verb").and_then(Value::as_str) != record.verb.as_deref() {
            return Err("ledger_verb_mismatch");
        }
        if let Some(stamped) = entry.get("reply_sha256").and_then(Value::as_str) {
            if record.reply_sha256.as_deref() != Some(stamped) {
                return Err("ledger_reply_sha256_mismatch");
            }
        }
        Ok(Confirmation::Ledger)
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Default)]
struct HistoryScan {
    worlds: Vec<World>,
    sessions: usize,
    observations: usize,
    ledger_confirmed: usize,
    spool_out_confirmed: usize,
    dropped: BTreeMap<&'static str, usize>,
    rejected: usize,
    latest: Option<(i64, String)>,
    windows: BTreeMap<String, BTreeSet<String>>,
}

#[cfg(target_arch = "wasm32")]
const HISTORY_ROOT_NODE: &str = "start";

#[cfg(target_arch = "wasm32")]
fn session_world(session_id: &str, records: &[ObservationRecord]) -> Option<World> {
    let mut nodes: BTreeMap<String, Node> = BTreeMap::new();
    let mut quality_sums: BTreeMap<String, f64> = BTreeMap::new();
    let mut previous = HISTORY_ROOT_NODE.to_string();
    nodes.insert(previous.clone(), Node { id: previous.clone(), score: 0.0, cost: 0, round: 0, children: Vec::new() });
    for record in records {
        let Some(verb) = record.verb.clone() else { continue };
        let Some(quality) = record.quality else { continue };
        let next_round = nodes.len() as u64;
        let node = nodes.entry(verb.clone()).or_insert_with(|| Node {
            id: verb.clone(),
            score: 0.0,
            cost: 0,
            round: next_round,
            children: Vec::new(),
        });
        node.cost += 1;
        *quality_sums.entry(verb.clone()).or_default() += quality;
        if let Some(parent) = nodes.get_mut(&previous) {
            if !parent.children.contains(&verb) {
                parent.children.push(verb.clone());
            }
        }
        previous = verb;
    }
    for (verb, sum) in quality_sums {
        if let Some(node) = nodes.get_mut(&verb) {
            node.score = sum / node.cost.max(1) as f64;
        }
    }
    (nodes.len() > 1).then(|| World { id: format!("session:{session_id}"), nodes })
}

#[cfg(target_arch = "wasm32")]
const SESSION_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

#[cfg(target_arch = "wasm32")]
fn newest_entry_mtime_ms(dir: &str) -> Option<i64> {
    dir_entry_names(dir)
        .iter()
        .filter_map(|name| crate::pkfs::stat(&format!("{dir}/{name}")))
        .filter_map(|s| s.get("mtime_ms").or_else(|| s.get("mtimeMs"))?.as_f64())
        .map(|ms| ms as i64)
        .max()
}

#[cfg(target_arch = "wasm32")]
fn open_row_sessions() -> Option<std::collections::HashSet<String>> {
    let (body, _, code) = super::prd::handle_list_full();
    if code != 0 {
        return None;
    }
    let doc: serde_json::Value = serde_json::from_str(&body).ok()?;
    Some(
        doc.get("items")?
            .as_array()?
            .iter()
            .filter(|row| {
                row.get("status")
                    .and_then(|s| s.as_str())
                    .is_some_and(super::prd::status_is_open)
            })
            .filter_map(|row| row.get("session_id")?.as_str().map(str::to_owned))
            .collect(),
    )
}

#[cfg(target_arch = "wasm32")]
fn prune_idle_sessions(now_ms: i64, keep: &str) {
    let Some(exempt) = open_row_sessions() else {
        return;
    };
    for session in dir_entry_names(".gm/dream-rsi") {
        if session.starts_with('.') || session == keep || exempt.contains(&session) {
            continue;
        }
        let dir = session_store_path(&session, "");
        if let Some(newest) = newest_entry_mtime_ms(&dir) {
            if now_ms.saturating_sub(newest) > SESSION_RETENTION_MS {
                crate::pkfs::remove_tree(&dir);
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn scan_history() -> HistoryScan {
    let mut ledger = LedgerCheck::load();
    let mut scan = HistoryScan::default();
    prune_idle_sessions(
        super::state::now_ms() as i64,
        &session_id().unwrap_or_default(),
    );
    let mut sessions = dir_entry_names(".gm/dream-rsi");
    sessions.retain(|name| !name.starts_with('.'));
    sessions.sort();
    for session_id in sessions {
        let (mut records, rejected) = session_history(&session_id);
        scan.rejected += rejected;
        if records.is_empty() {
            continue;
        }
        scan.sessions += 1;
        scan.observations += records.len();
        scan.windows.insert(
            session_id.clone(),
            records.iter().filter_map(|record| record.dispatch_id.clone()).collect(),
        );
        records.retain(|record| match ledger.verdict(&session_id, record) {
            Ok(Confirmation::Ledger) | Ok(Confirmation::Retained) => {
                scan.ledger_confirmed += 1;
                true
            }
            Err(reason) => {
                *scan.dropped.entry(reason).or_default() += 1;
                false
            }
        });
        records.sort_by_key(|record| record.ts.unwrap_or(0));
        if let Some(newest) = records.last() {
            let candidate = (newest.ts.unwrap_or(0), newest.dispatch_id.clone().unwrap_or_default());
            if scan.latest.as_ref().map_or(true, |latest| candidate > *latest) {
                scan.latest = Some(candidate);
            }
        }
        if let Some(world) = session_world(&session_id, &records) {
            scan.worlds.push(world);
        }
    }
    scan
}

#[cfg(target_arch = "wasm32")]
const DEFAULT_INCUMBENT_POLICY_ID: &str = "pi0-default";

#[cfg(target_arch = "wasm32")]
fn incumbent_policy(owner: &str) -> Policy {
    active_strategy(Some(owner))
        .get("policy")
        .and_then(|policy| parse_policy(policy).ok())
        .filter(|policy| policy.roots.iter().all(|root| root == HISTORY_ROOT_NODE))
        .unwrap_or_else(|| Policy {
            id: DEFAULT_INCUMBENT_POLICY_ID.to_string(),
            roots: vec![HISTORY_ROOT_NODE.to_string()],
            max_nodes: 16,
            attempt: None,
            group_size: None,
            stop: None,
        })
}

#[cfg(target_arch = "wasm32")]
fn dream_candidates(incumbent: &Policy) -> Vec<Policy> {
    let variant = |label: &str, edit: &dyn Fn(&mut Policy)| {
        let mut candidate = incumbent.clone();
        edit(&mut candidate);
        candidate.id = format!("{}.{label}", incumbent.id);
        candidate
    };
    let mut candidates = vec![
        incumbent.clone(),
        variant("half-nodes", &|policy: &mut Policy| policy.max_nodes = (policy.max_nodes / 2).max(1)),
        variant("double-nodes", &|policy: &mut Policy| policy.max_nodes = policy.max_nodes.saturating_mul(2)),
        variant("group-2", &|policy: &mut Policy| policy.group_size = Some(2)),
        variant("group-4", &|policy: &mut Policy| policy.group_size = Some(4)),
        variant("stop-converged", &|policy: &mut Policy| policy.stop = Some(Stop::Converged)),
        variant("stop-exhausted", &|policy: &mut Policy| policy.stop = Some(Stop::Exhausted)),
        variant("attempt-2", &|policy: &mut Policy| policy.attempt = Some(2)),
    ];
    let mut seen = BTreeSet::new();
    candidates.retain(|policy| {
        seen.insert((policy.roots.clone(), policy.max_nodes, policy.group_size, policy.stop.clone(), policy.attempt.unwrap_or(1).max(1)))
    });
    candidates
}

#[cfg(target_arch = "wasm32")]
fn policy_json(policy: &Policy) -> Value {
    json!({
        "id": policy.id, "roots": policy.roots, "max_nodes": policy.max_nodes,
        "group_size": policy.group_size, "stop": policy.stop.map(Stop::name), "attempt": policy.attempt,
    })
}

#[cfg(target_arch = "wasm32")]
pub const POSTCONDITION_FAILED: &str = "dream-replay-cycle postcondition failed";

#[cfg(target_arch = "wasm32")]
pub const DEFAULT_CYCLE_BETA1: f64 = 0.01;

#[cfg(target_arch = "wasm32")]
pub const DEFAULT_CYCLE_BETA2: f64 = 0.05;

#[cfg(target_arch = "wasm32")]
pub struct HistoryRound {
    pub latest_dispatch_id: Option<String>,
    pub summary: Value,
    pub lost_cursor: Option<String>,
    pub no_replayable_worlds: bool,
}

#[cfg(target_arch = "wasm32")]
pub(super) fn history_round(owner: &str, cursor: Option<&str>, beta1: f64, beta2: f64) -> Result<HistoryRound, String> {
    let scan = scan_history();
    let latest_dispatch_id = scan.latest.as_ref().map(|(_, id)| id.clone());
    let lost_cursor = cursor
        .filter(|id| !scan.windows.get(owner).is_some_and(|ids| ids.contains(*id)))
        .map(str::to_owned);
    let incumbent = incumbent_policy(owner);
    let candidates = dream_candidates(&incumbent);
    let mut defects: Vec<&str> = Vec::new();
    let incumbent_present = candidates.iter().any(|policy| policy.id == incumbent.id);
    if !incumbent_present {
        return Err("dream-replay-cycle precondition failed: incumbent absent from candidates".into());
    }
    let history = json!({
        "sessions": scan.sessions, "observations": scan.observations,
        "ledger_confirmed": scan.ledger_confirmed, "spool_out_confirmed": scan.spool_out_confirmed,
        "dropped": scan.dropped, "rejected": scan.rejected, "worlds": scan.worlds.len(),
    });
    if scan.worlds.is_empty() {
        return Ok(HistoryRound {
            latest_dispatch_id: latest_dispatch_id.clone(),
            summary: json!({ "history": history, "incumbent_policy_id": incumbent.id, "candidates": candidates.len() }),
            lost_cursor: lost_cursor.clone(),
            no_replayable_worlds: true,
        });
    }
    let candidate_count = candidates.len();
    let replay = evaluate_worlds(incumbent.id.clone(), candidates.clone(), scan.worlds, beta1, beta2)?;
    let baseline_score = replay.get("baseline_score").and_then(Value::as_f64).ok_or("dream-replay-cycle baseline score missing")?;
    let selected_id = replay.get("selected_policy_id").and_then(Value::as_str).unwrap_or(&incumbent.id).to_string();
    let selected_score = replay.get("selected_score").and_then(Value::as_f64).ok_or("dream-replay-cycle selected score missing")?;
    let ranked_score = |id: &str| {
        replay
            .get("rankings")
            .and_then(Value::as_array)
            .and_then(|rankings| rankings.iter().find(|ranking| ranking.get("policy_id").and_then(Value::as_str) == Some(id)))
            .and_then(|ranking| ranking.get("score").and_then(Value::as_f64))
    };
    let never_worse = matches!(
        (ranked_score(&selected_id), ranked_score(&incumbent.id)),
        (Some(selected), Some(incumbent_score)) if selected >= incumbent_score && selected_score >= baseline_score
    );
    if !never_worse {
        defects.push("postcondition_selected_below_incumbent");
    }
    let mut rankings: Vec<Value> = replay
        .get("rankings")
        .and_then(Value::as_array)
        .map(|rankings| {
            rankings
                .iter()
                .map(|ranking| json!({ "policy_id": ranking.get("policy_id"), "score": ranking.get("score"), "cost": ranking.get("cost") }))
                .collect()
        })
        .unwrap_or_default();
    if rankings.len() != candidate_count {
        defects.push("postcondition_rankings_incomplete");
    }
    if !defects.is_empty() {
        return Err(format!("{POSTCONDITION_FAILED}: {}", defects.join(", ")));
    }
    rankings.sort_by(|left, right| {
        right.get("score").and_then(Value::as_f64).partial_cmp(&left.get("score").and_then(Value::as_f64)).unwrap_or(std::cmp::Ordering::Equal)
    });
    let improved = selected_id != incumbent.id && selected_score > baseline_score;
    let selected_id = if improved { selected_id } else { incumbent.id.clone() };
    let selected_policy = candidates.iter().find(|policy| policy.id == selected_id).map(policy_json);
    Ok(HistoryRound {
        latest_dispatch_id,
        lost_cursor,
        no_replayable_worlds: false,
        summary: json!({
            "selection": if improved { "propose-challenger" } else { "keep-incumbent" },
            "incumbent_policy_id": incumbent.id,
            "incumbent_included": incumbent_present,
            "selected_policy_id": selected_id,
            "selected_policy": selected_policy,
            "baseline_score": baseline_score,
            "incumbent_score": baseline_score,
            "selected_score": selected_score,
            "improved": improved,
            "never_worse": never_worse,
            "candidates": candidate_count,
            "rankings": rankings,
            "beta1": beta1,
            "beta2": beta2,
            "history": history,
            "contract": { "pre_incumbent_in_candidates": incumbent_present, "post_selected_ge_baseline": never_worse, "defects": defects },
        }),
    })
}

#[cfg(target_arch = "wasm32")]
pub struct Outcome {
    pub exit_code: i64,
    pub output_bytes: u64,
    gate_drift: bool,
    gate_id: Option<String>,
    predicate: Option<String>,
    first_error_line: Option<String>,
    failure: FailureClass,
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn outcome(result: &Value) -> Outcome {
    let decoded = result
        .get("data")
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .filter(|value| value.is_object());
    let data = decoded
        .as_ref()
        .or_else(|| result.get("data").filter(|value| value.is_object()))
        .unwrap_or(result);
    let flag = |key: &str| {
        result
            .get(key)
            .and_then(Value::as_bool)
            .or_else(|| data.get(key).and_then(Value::as_bool))
    };
    let exit_code = data
        .get("exit_code")
        .and_then(Value::as_i64)
        .unwrap_or(if flag("ok") == Some(true) { 0 } else { 1 });
    let failure = failure_class(result, exit_code);
    Outcome {
        exit_code,
        output_bytes: dispatch_output_bytes(result),
        gate_drift: failure.error_code().is_some_and(is_gate_drift_code),
        gate_id: reply_field(result, "gate_id"),
        predicate: reply_field(result, "predicate"),
        first_error_line: error_line(result, exit_code),
        failure,
    }
}

#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
pub(crate) fn observe_recorded(
    outcome: Outcome,
    dispatch_id: &str,
    session_id: &str,
    verb: &str,
    fingerprint: &str,
    reply: &str,
    lean_node: Option<&str>,
    run_end: bool,
) {
    observe_dispatch(ObservedDispatch {
        session_id,
        dispatch_id,
        verb,
        fingerprint,
        exit_code: outcome.exit_code,
        output_bytes: Some(outcome.output_bytes),
        gate_drift: outcome.gate_drift,
        gate_id: outcome.gate_id.as_deref(),
        predicate: outcome.predicate.as_deref(),
        first_error_line: outcome.first_error_line.as_deref(),
        reply,
        lean_node,
        failure: outcome.failure,
        run_end,
    });
}

#[cfg(target_arch = "wasm32")]
use super::NON_DECISION_VERBS;

#[cfg(target_arch = "wasm32")]
fn observe_dispatch(observed: ObservedDispatch<'_>) {
    let ObservedDispatch {
        session_id,
        dispatch_id,
        verb,
        fingerprint,
        exit_code,
        output_bytes,
        gate_drift,
        gate_id,
        predicate,
        first_error_line,
        reply,
        lean_node,
        failure,
        run_end,
    } = observed;
    if verb.starts_with("dream-") || NON_DECISION_VERBS.contains(&verb) {
        return;
    }
    let failure = failure.for_exit(exit_code);
    let path = session_store_path(session_id, OBSERVATIONS_FILE);
    let ts = super::state::now_ms() as i64;
    rotate_observations_if_full(session_id, &path, ts);
    if recover_observations_tail(&path).is_none() {
        return;
    }
    let (prd_open_count, mutable_open_count) = super::instructions::open_row_counts();
    let record = ObservationRecord {
        dispatch_id: Some(dispatch_id.to_string()),
        verb: Some(verb.to_string()),
        fingerprint: Some(fingerprint.to_string()),
        exit_code: Some(exit_code),
        gate_drift: Some(gate_drift),
        gate_id: gate_id.map(str::to_owned),
        predicate: predicate.map(str::to_owned),
        first_error_line: first_error_line.map(str::to_owned),
        prd_open_count,
        mutable_open_count: Some(mutable_open_count),
        quality: Some(outcome_score(exit_code, failure.no_match(), output_bytes)),
        ts: Some(ts),
        reply_sha256: Some(reply_sha256_hex(reply)),
        lean_node: lean_node.map(str::to_owned),
        error_code: failure.error_code().map(str::to_owned),
        no_match: failure.no_match(),
        failure_class: Some(failure.name().to_owned()),
        run_end,
        policy_id: Some(incumbent_policy(session_id).id),
        ..Default::default()
    };
    let Ok(line) = serde_json::to_string(&record) else {
        return;
    };
    if crate::pkfs::append(&path, &format!("{line}\n")) {
        bump_live_count(session_id);
    }
}

fn is_failure_reply(map: &serde_json::Map<String, Value>) -> bool {
    map.get("ok") == Some(&Value::Bool(false)) || map.contains_key("error") || map.contains_key("error_code")
}

fn is_blank(value: &Value) -> bool {
    value.is_null() || value.as_str() == Some("") || value.as_array().is_some_and(Vec::is_empty)
}

fn insert_if_present(out: &mut serde_json::Map<String, Value>, key: &str, value: Option<&Value>) {
    if let Some(value) = value.filter(|value| !is_blank(value)) {
        out.insert(key.to_string(), value.clone());
    }
}

pub(super) fn compact_active_strategy(strategy: Value) -> Value {
    let Some(map) = strategy.as_object() else {
        return strategy;
    };
    if is_failure_reply(map) {
        return strategy;
    }
    let evidence_rows = map.get("evidence").and_then(Value::as_array).map_or(0, Vec::len);
    let mut out = serde_json::Map::new();
    for (wire, raw) in [
        ("selection", "selection"),
        ("observations", "observation_count"),
        ("succeeded", "successful_dispatch_count"),
        ("failed", "failed_dispatch_count"),
        ("unverified", "unverified_dispatch_count"),
        ("unscored", "unscored_dispatch_count"),
        ("gate_drift_failures", "gate_drift_failure_count"),
    ] {
        insert_if_present(&mut out, wire, map.get(raw));
    }
    insert_if_present(
        &mut out,
        "selected_policy_id",
        map.get("selected_policy_id")
            .or_else(|| map.get("policy").and_then(|policy| policy.get("id"))),
    );
    out.insert("evidence_rows".to_string(), json!(evidence_rows));
    Value::Object(out)
}

pub(super) fn compact_persisted_replay(replay: Value) -> Value {
    let Some(map) = replay.as_object() else {
        return replay;
    };
    if is_failure_reply(map) {
        return replay;
    }
    let replay_rows = map.get("replays").and_then(Value::as_array).map_or(0, Vec::len);
    let mut out = serde_json::Map::new();
    for key in ["ok", "selection", "score"] {
        insert_if_present(&mut out, key, map.get(key));
    }
    out.insert("replay_rows".to_string(), json!(replay_rows));
    Value::Object(out)
}

#[cfg(target_arch = "wasm32")]
pub(super) fn active_strategy(session_id: Option<&str>) -> Value {
    let Some(session_id) = session_id.filter(|id| !id.trim().is_empty()) else {
        return Value::Null;
    };
    let read = |owner: &str| -> Option<Value> {
        crate::pkfs::read_to_string(&session_store_path(owner, &format!("{ACTIVE_STRATEGY_FILE}.json")))
            .and_then(|raw| serde_json::from_str(&raw).ok())
    };
    read(session_id).or_else(|| read(PROJECT_WIDE_SESSION)).unwrap_or(Value::Null)
}

#[cfg(target_arch = "wasm32")]
pub const PROJECT_WIDE_SESSION: &str = "_any-session";

#[cfg(target_arch = "wasm32")]
pub const PROBE_STRATEGY_FILE: &str = "probe-strategy.json";

#[cfg(target_arch = "wasm32")]
pub(super) fn update_probe(owner: &str, fields: &Value) {
    let path = session_store_path(owner, PROBE_STRATEGY_FILE);
    let baseline = fields.get("baseline_score").and_then(Value::as_f64);
    if crate::pkfs::exists(&path) {
        let probe = crate::pkfs::read_to_string(&path).and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        let probe_score = probe
            .as_ref()
            .and_then(|probe| probe.get("policy_id")?.as_str())
            .and_then(|policy| {
                fields
                    .get("rankings")?
                    .as_array()?
                    .iter()
                    .find(|ranking| ranking.get("policy_id").and_then(Value::as_str) == Some(policy))?
                    .get("score")?
                    .as_f64()
            });
        let dampen = matches!((probe_score, baseline), (Some(score), Some(base)) if score < base);
        if dampen {
            crate::pkfs::remove_file(&path);
        }
        return;
    }
    let improved = fields.get("improved").and_then(Value::as_bool).unwrap_or(false);
    let never_worse = fields.get("never_worse").and_then(Value::as_bool).unwrap_or(false);
    let observations = fields
        .get("history")
        .and_then(|history| history.get("observations"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if improved && never_worse && observations >= INSUFFICIENT_RECORD_THRESHOLD {
        let probe = json!({
            "kind": "probe-strategy",
            "scope": "session",
            "owner_session_id": owner,
            "policy_id": fields.get("selected_policy_id"),
            "baseline_score": baseline,
            "selected_score": fields.get("selected_score"),
            "n_observations": observations,
            "ts": super::state::now_ms() as i64,
        });
        crate::pkfs::write(&path, &probe.to_string());
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn persisted_cycle_result(session_id: Option<&str>) -> Value {
    let Some(session_id) = session_id.filter(|id| !id.trim().is_empty()) else {
        return Value::Null;
    };
    crate::pkfs::read_to_string(&session_store_path(session_id, &format!("{CYCLE_RESULT_FILE}.json")))
    .and_then(|raw| serde_json::from_str(&raw).ok())
    .unwrap_or(Value::Null)
}

#[cfg(target_arch = "wasm32")]
pub(super) fn persisted_probe(session_id: &str) -> Value {
    crate::pkfs::read_to_string(&session_store_path(session_id, PROBE_STRATEGY_FILE))
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null)
}

#[cfg(target_arch = "wasm32")]
fn session_id() -> Result<String, String> {
    crate::orchestrator::state::dispatch_session_id()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| "Dream-RSI requires a gm session ID".to_string())
}

#[cfg(target_arch = "wasm32")]
const DISPATCH_PAYLOAD_KEYS: [&str; 9] = [
    "data", "result", "output", "text", "matches", "edges", "chunks", "rows", "stdout",
];

#[cfg(target_arch = "wasm32")]
fn dispatch_output_bytes(result: &Value) -> u64 {
    fn payload_text_bytes(value: &Value) -> u64 {
        match value {
            Value::String(text) if text.trim().is_empty() => 0,
            Value::String(text) => text.len() as u64,
            Value::Array(items) => items.iter().map(payload_text_bytes).sum(),
            Value::Object(fields) => fields
                .iter()
                .filter(|(key, _)| DISPATCH_PAYLOAD_KEYS.contains(&key.as_str()))
                .map(|(_, child)| payload_text_bytes(child))
                .sum(),
            Value::Null | Value::Bool(_) | Value::Number(_) => 0,
        }
    }
    payload_text_bytes(result)
}

#[cfg(target_arch = "wasm32")]
fn outcome_score(exit_code: i64, no_match: bool, output_bytes: Option<u64>) -> f64 {
    match (exit_code, no_match, output_bytes) {
        (0, _, Some(0)) => 0.5,
        (0, _, _) => 1.0,
        (_, true, _) => 0.5,
        _ => 0.0,
    }
}

#[cfg(target_arch = "wasm32")]
pub const ACTIVE_STRATEGY_FILE: &str = "active-strategy";

#[cfg(target_arch = "wasm32")]
pub const CYCLE_RESULT_FILE: &str = "cycle-result";
pub const OBSERVATIONS_FILE: &str = "observations.json";

#[cfg(target_arch = "wasm32")]
pub const INDEX_PATH: &str = ".gm/dream-rsi/index.json";

#[cfg(target_arch = "wasm32")]
pub const INDEX_SCHEMA_VERSION: u32 = 1;

#[cfg(target_arch = "wasm32")]
fn write_index() {
    let index = json!({
        "schema_version": INDEX_SCHEMA_VERSION,
        "files": {
            "observations": OBSERVATIONS_FILE,
            "active_strategy": format!("{ACTIVE_STRATEGY_FILE}.json"),
            "cycle_result": format!("{CYCLE_RESULT_FILE}.json"),
        },
        "record_kinds": {
            "cycle_result": "cycle-result",
        },
        "fields": ["kind", "source", "owner_session_id", "ts"],
    });
    let _ = crate::pkfs::write(INDEX_PATH, &index.to_string());
}

#[cfg(target_arch = "wasm32")]
pub enum ReplayFile {
    Cycle,
}

#[cfg(target_arch = "wasm32")]
impl ReplayFile {
    fn name(&self) -> &'static str {
        match self {
            ReplayFile::Cycle => CYCLE_RESULT_FILE,
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub struct ReplaySource(pub &'static str);

#[cfg(target_arch = "wasm32")]
pub(super) fn persist_replay_result(
    file: ReplayFile,
    source: ReplaySource,
    fields: serde_json::Map<String, Value>,
) -> Result<String, String> {
    let owner = session_id()?;
    let name = file.name();
    let path = current_session_store_path(name)?;
    let kind = match file {
        ReplayFile::Cycle => "cycle-result",
    };
    let mut record = fields;
    record.insert("kind".into(), json!(kind));
    record.insert("source".into(), json!(source.0));
    record.insert("owner_session_id".into(), json!(owner));
    record.insert("ts".into(), json!(super::state::now_ms() as i64));
    if crate::pkfs::write(&path, &Value::Object(record).to_string()) {
        write_index();
        Ok(path)
    } else {
        Err(format!("could not write replay result to {path}"))
    }
}
