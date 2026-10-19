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
pub const MAX_PART_A_PROPOSALS: usize = 3;
pub const MAX_PART_B_PROPOSALS: usize = 5;

#[derive(Debug, Clone, PartialEq)]
pub enum DreamRsiStatus {
    Proposed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerbStat {
    pub verb: String,
    pub count: u64,
    pub failure_rate: f64,
    pub gate_drift_rate: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerbProposal {
    pub verb: String,
    pub change: String,
    pub evidence: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PartA {
    Measured {
        records: u64,
        per_verb: Vec<VerbStat>,
        proposals: Vec<VerbProposal>,
    },
    Insufficient {
        records: u64,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RejectedWalk {
    pub node: String,
    pub missing_dispatch_ids: Vec<String>,
    pub reason: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeProposal {
    pub node: String,
    pub walk_when: String,
    pub evidence_that_counts: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PartB {
    pub traversable_nodes: u64,
    pub nodes_verified: u64,
    pub backreference_edges: u64,
    pub backreferences_verified: u64,
    pub forward_edge_coverage_approx: f64,
    pub rejected_walks: Vec<RejectedWalk>,
    pub proposals: Vec<NodeProposal>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DreamRsiResult {
    pub status: DreamRsiStatus,
    pub part_a: PartA,
    pub part_b: PartB,
}

fn string_field(value: &Value, field: &str) -> Result<String, String> {
    let value = value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("dream-replay requires non-empty {field}"))?;
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(format!(
            "dream-replay {field} must be an opaque ASCII identifier"
        ));
    }
    Ok(value.to_string())
}

fn array_field<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>, String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("dream-replay requires array {field}"))
}

fn nonneg_f64_field(value: &Value, field: &str) -> Result<f64, String> {
    value
        .get(field)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .ok_or_else(|| format!("dream-replay requires non-negative finite {field}"))
}

fn parse_policy(value: &Value) -> Result<Policy, String> {
    let id = string_field(value, "id")?;
    let roots = array_field(value, "roots")?
        .iter()
        .map(|root| {
            root.as_str()
                .filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("dream-replay policy {id} has an empty root"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if roots.is_empty() {
        return Err(format!(
            "dream-replay policy {id} requires at least one root"
        ));
    }
    let max_nodes = value
        .get("max_nodes")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("dream-replay policy {id} requires positive safe max_nodes"))?;
    let group_size = match value.get("group_size") {
        None | Some(Value::Null) => None,
        Some(raw) => Some(
            raw.as_u64()
                .filter(|value| *value > 0)
                .ok_or_else(|| format!("dream-replay policy {id} field group_size must be a positive integer when present"))?,
        ),
    };
    let stop = match value.get("stop") {
        None | Some(Value::Null) => None,
        Some(raw) => Some(
            raw.as_str()
                .and_then(Stop::parse)
                .ok_or_else(|| {
                    let names = Stop::NAMES.map(|(name, _)| name);
                    format!("dream-replay policy {id} field stop must be one of {names:?} when present")
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

fn parse_world(value: &Value) -> Result<World, String> {
    let id = string_field(value, "id")?;
    let mut nodes = BTreeMap::new();
    for (index, raw) in array_field(value, "nodes")?.iter().enumerate() {
        let node_id = string_field(raw, "id")?;
        if nodes.contains_key(&node_id) {
            return Err(format!("dream-replay world {id} repeats node {node_id}"));
        }
        let score = raw
            .get("score")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("dream-replay node {node_id} requires finite score"))?;
        let cost = raw.get("cost").and_then(Value::as_u64).ok_or_else(|| {
            format!("dream-replay node {node_id} requires non-negative integer cost")
        })?;
        let round = raw
            .get("round")
            .and_then(Value::as_u64)
            .unwrap_or(index as u64);
        let children = array_field(raw, "children")?
            .iter()
            .map(|child| {
                child
                    .as_str()
                    .filter(|value| !value.trim().is_empty())
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| format!("dream-replay node {node_id} has an empty child"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        nodes.insert(
            node_id.clone(),
            Node {
                id: node_id,
                score,
                cost,
                round,
                children,
            },
        );
    }
    if nodes.is_empty() {
        return Err(format!(
            "dream-replay world {id} requires at least one node"
        ));
    }
    for node in nodes.values() {
        for child in &node.children {
            if !nodes.contains_key(child) {
                return Err(format!(
                    "dream-replay world {id} node {} references unobserved child {child}",
                    node.id
                ));
            }
        }
    }
    Ok(World { id, nodes })
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
                    "dream-replay policy {} asks world {} for unobserved node {id}",
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
                .ok_or_else(|| "dream-replay cost overflow".to_string())?;
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
            "dream-replay policy {} observed no nodes in world {}",
            policy.id, world.id
        ));
    }
    let rounds = if policy.group_size.is_some() { waves } else { recorded_rounds.len() };
    let parallelism_bonus = observed.len() as f64 / rounds.max(1) as f64;
    let replay_score = quality - beta1 * cost as f64 + beta2 * parallelism_bonus;
    if !replay_score.is_finite() {
        return Err(format!(
            "dream-replay policy {} replay score in world {} is non-finite",
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
            return Err(format!("dream-replay repeats policy {}", policy.id));
        }
    }
    if !policy_ids.contains(&baseline_id) {
        return Err(format!(
            "dream-replay baseline_policy_id {baseline_id} is absent from policies"
        ));
    }
    if worlds.is_empty() {
        return Err("dream-replay requires at least one sealed world".to_string());
    }
    if let Some(position) = policies.iter().position(|policy| policy.id == baseline_id) {
        let incumbent = policies.remove(position);
        policies.insert(0, incumbent);
    }
    let mut rankings = Vec::new();
    for policy in &policies {
        let replays = worlds
            .iter()
            .map(|world| evaluate_policy(world, policy, beta1, beta2))
            .collect::<Result<Vec<_>, _>>()?;
        let scores = replays
            .iter()
            .map(|replay| {
                replay
                    .get("replay_score")
                    .and_then(Value::as_f64)
                    .ok_or_else(|| {
                        format!("dream-replay policy {} replay lacks a numeric replay_score", policy.id)
                    })
            })
            .collect::<Result<Vec<f64>, String>>()?;
        let score = scores.iter().sum::<f64>() / scores.len() as f64;
        if !score.is_finite() {
            return Err(format!("dream-replay policy {} score is non-finite", policy.id));
        }
        let cost = replays
            .iter()
            .map(|replay| {
                replay.get("cost").and_then(Value::as_u64).ok_or_else(|| {
                    format!("dream-replay policy {} replay lacks a numeric cost", policy.id)
                })
            })
            .sum::<Result<u64, String>>()?;
        rankings.push(json!({
            "policy_id": policy.id,
            "score": score,
            "cost": cost,
            "attempt": policy.attempt.unwrap_or(1).max(1),
            "group_size": policy.group_size,
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
        .ok_or_else(|| "dream-replay baseline ranking missing".to_string())?;
    let baseline_score = baseline
        .get("score")
        .and_then(Value::as_f64)
        .ok_or_else(|| "dream-replay baseline score missing".to_string())?;
    let selected = rankings
        .iter()
        .filter(|ranking| {
            worlds.len() >= PROMOTION_MIN_WORLDS
                && ranking
                    .get("score")
                    .and_then(Value::as_f64)
                    .unwrap_or(f64::NEG_INFINITY)
                    > baseline_score + PROMOTION_MARGIN
        })
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
        } else if worlds.len() < PROMOTION_MIN_WORLDS {
            "insufficient_worlds"
        } else {
            "below_margin"
        },
        "promotion_margin": PROMOTION_MARGIN,
        "n_observations": worlds.len(),
        "domain": if worlds.len() < PROMOTION_MIN_WORLDS { "disorder" } else { "order" },
        "beta1": beta1,
        "beta2": beta2,
        "rankings": rankings,
    }))
}

#[cfg(target_arch = "wasm32")]
fn session_store_path(name: &str) -> Result<String, String> {
    Ok(format!(".gm/dream-rsi/{}/{}.json", session_id()?, name))
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
        FailureClass::Unclassified
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    run_end: bool,
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
        !self.succeeded() && self.gate_drift.unwrap_or(true)
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
    let mut names: Vec<(i64, String)> = dir_entry_names(&format!(".gm/dream-rsi/{session_id}"))
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
    let path = format!(".gm/dream-rsi/{session_id}/{OBSERVATIONS_ARCHIVE_PREFIX}{ts}.json");
    if crate::pkfs::exists(&path) || !crate::pkfs::write(&path, raw) {
        return None;
    }
    let names = archive_names(session_id);
    for stale in names.iter().take(names.len().saturating_sub(OBSERVATIONS_ARCHIVE_MAX_CHUNKS)) {
        crate::pkfs::remove_file(&format!(".gm/dream-rsi/{session_id}/{stale}"));
    }
    Some(path)
}

#[cfg(target_arch = "wasm32")]
fn live_count_path(session_id: &str) -> String {
    format!(".gm/dream-rsi/{session_id}/{OBSERVATIONS_LIVE_COUNT_FILE}")
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
        if let Some(raw) = crate::pkfs::read_to_string(&format!(".gm/dream-rsi/{session_id}/{name}")) {
            absorb(&raw);
        }
    }
    if let Some(raw) = recover_observations_tail(&format!(".gm/dream-rsi/{session_id}/observations.json")) {
        absorb(&raw);
    }
    (records, rejected)
}

#[cfg(target_arch = "wasm32")]
const SPOOL_OUT_DIR: &str = ".gm/exec-spool/out";

#[cfg(target_arch = "wasm32")]
#[derive(Default)]
struct LedgerCheck {
    entries: std::collections::HashMap<String, Value>,
    unreadable: Option<&'static str>,
    evicted_possible: bool,
    newest_ts: u64,
    spool_out_names: Option<Vec<String>>,
    spool_out_ids_by_session: std::collections::HashMap<String, BTreeSet<String>>,
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy, PartialEq)]
enum Confirmation {
    Ledger,
    SpoolOut,
    Retained,
}

#[cfg(target_arch = "wasm32")]
fn reply_dispatch_ids(text: &str) -> impl Iterator<Item = String> + '_ {
    const NEEDLE: &str = "\"dispatch_id\":\"";
    text.match_indices(NEEDLE).filter_map(move |(start, _)| {
        let rest = &text[start + NEEDLE.len()..];
        rest.find('"').map(|end| rest[..end].to_owned())
    })
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

    fn spool_out_confirms(&mut self, session_id: &str, dispatch_id: &str) -> bool {
        if !self.spool_out_ids_by_session.contains_key(session_id) {
            let names = self.spool_out_names.get_or_insert_with(|| {
                dir_entry_names(SPOOL_OUT_DIR).into_iter().filter(|name| name.ends_with(".json")).collect()
            });
            let marker = format!("-{session_id}-");
            let ids = names
                .iter()
                .filter(|name| name.contains(&marker))
                .filter_map(|name| crate::pkfs::read_to_string(&format!("{SPOOL_OUT_DIR}/{name}")))
                .flat_map(|text| reply_dispatch_ids(&text).collect::<Vec<_>>())
                .collect();
            self.spool_out_ids_by_session.insert(session_id.to_owned(), ids);
        }
        self.spool_out_ids_by_session.get(session_id).is_some_and(|ids| ids.contains(dispatch_id))
    }

    fn verdict(&mut self, session_id: &str, record: &ObservationRecord) -> Result<Confirmation, &'static str> {
        let dispatch_id = record.dispatch_id.as_deref().ok_or("no_dispatch_id")?;
        let Some(entry) = self.entries.get(dispatch_id) else {
            let aged_out = record.ts.is_some_and(|ts| u64::try_from(ts).is_ok_and(|ts| ts.saturating_add(crate::dispatch_ledger::RETAIN_MS) < self.newest_ts));
            if (self.evicted_possible || aged_out) && self.unreadable.is_none() {
                return Ok(Confirmation::Retained);
            }
            if self.spool_out_confirms(session_id, dispatch_id) {
                return Ok(Confirmation::SpoolOut);
            }
            return Err(self.unreadable.unwrap_or("unconfirmed_by_ledger_or_spool_out"));
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
        let dir = format!(".gm/dream-rsi/{session}");
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
            Ok(Confirmation::SpoolOut) => {
                scan.spool_out_confirmed += 1;
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
    failure: FailureClass,
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn outcome(result: &Value) -> Outcome {
    let data = result.get("data").filter(|value| value.is_object()).unwrap_or(result);
    let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(true)
        && data.get("ok").and_then(Value::as_bool).unwrap_or(true);
    let exit_code = data.get("exit_code").and_then(Value::as_i64).unwrap_or(if ok { 0 } else { 1 });
    let failure = failure_class(result, exit_code);
    Outcome {
        exit_code,
        output_bytes: dispatch_output_bytes(result),
        gate_drift: failure.error_code().is_some_and(is_gate_drift_code),
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
        reply,
        lean_node,
        failure: outcome.failure,
        run_end,
    });
}

#[cfg(target_arch = "wasm32")]
const NON_DECISION_VERBS: &[&str] = &[
    "unknown-add",
    "unknown-resolve",
    "unknown-list",
    "unknown-defer",
    "mutable-add",
    "mutable-resolve",
    "mutable-list",
    "mutable-defer",
];

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
        reply,
        lean_node,
        failure,
        run_end,
    } = observed;
    if verb.starts_with("dream-") || NON_DECISION_VERBS.contains(&verb) {
        return;
    }
    let failure = failure.for_exit(exit_code);
    let path = format!(".gm/dream-rsi/{session_id}/observations.json");
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
        prd_open_count: Some(prd_open_count),
        mutable_open_count: Some(mutable_open_count),
        quality: Some(outcome_score(exit_code, failure.no_match(), output_bytes)),
        ts: Some(ts),
        reply_sha256: Some(reply_sha256_hex(reply)),
        lean_node: lean_node.map(str::to_owned),
        error_code: failure.error_code().map(str::to_owned),
        no_match: failure.no_match(),
        failure_class: Some(failure.name().to_owned()),
        run_end,
        ..Default::default()
    };
    let Ok(line) = serde_json::to_string(&record) else {
        return;
    };
    if crate::pkfs::append(&path, &format!("{line}\n")) {
        if let Some(count) = read_live_count(session_id) {
            write_live_count(session_id, count + 1);
        }
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
    crate::pkfs::read_to_string(&format!(
        ".gm/dream-rsi/{session_id}/{ACTIVE_STRATEGY_FILE}.json"
    ))
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null)
}

#[cfg(target_arch = "wasm32")]
fn automatic_replay(session_id: Option<&str>) -> Value {
    let Some(session_id) = session_id.filter(|id| !id.trim().is_empty()) else {
        return Value::Null;
    };
    let (observations, parse_rejected) = session_history(session_id);
    if observations.is_empty() && parse_rejected == 0 {
        return Value::Null;
    }
    let mut ledger = LedgerCheck::load();
    let mut dropped: BTreeMap<&'static str, usize> = BTreeMap::new();
    if parse_rejected > 0 {
        dropped.insert("unparseable", parse_rejected);
    }
    let mut replays = Vec::new();
    for observation in observations.iter().rev() {
        if !observation.succeeded() && !observation.is_gate_drift_failure() {
            *dropped.entry("non_gate_failure").or_default() += 1;
            continue;
        }
        if let Err(reason) = ledger.verdict(session_id, observation) {
            *dropped.entry(reason).or_default() += 1;
            continue;
        }
        let Some(quality) = observation.quality else {
            *dropped.entry("unscored").or_default() += 1;
            continue;
        };
        let Some(prd_open) = observation.prd_open_count else {
            *dropped.entry("prd_count_unknown").or_default() += 1;
            continue;
        };
        let cost = prd_open
            + observation.mutable_open_count.unwrap_or(0)
            + 1;
        let replay_score = quality - DEFAULT_CYCLE_BETA1 * cost as f64;
        replays.push(json!({ "dispatch_id": observation.dispatch_id, "verb": observation.verb, "fingerprint": observation.fingerprint, "score": replay_score, "quality": quality, "cost": cost }));
    }
    if replays.is_empty() {
        return json!({ "ok": true, "selection": Value::Null, "replay_count": 0, "observations": observations.len(), "dropped": dropped, "replays": [] });
    }
    let score = replays
        .iter()
        .filter_map(|entry| entry.get("score").and_then(Value::as_f64))
        .sum::<f64>();
    let selected = if replays
        .iter()
        .any(|entry| entry.get("quality").and_then(Value::as_f64) == Some(0.0))
    {
        SELECTION_REPLAY_RECORDED_SUCCESSES_FIRST
    } else {
        SELECTION_CONTINUE_CURRENT_EXPLORATION
    };
    let replay_count = replays.len();
    replays.truncate(AUTOMATIC_REPLAY_SAMPLE);
    json!({ "ok": true, "selection": selected, "score": score, "replay_count": replay_count, "observations": observations.len(), "dropped": dropped, "replays": replays })
}

#[cfg(target_arch = "wasm32")]
pub(super) fn persisted_cycle_result(session_id: Option<&str>) -> Value {
    let Some(session_id) = session_id.filter(|id| !id.trim().is_empty()) else {
        return Value::Null;
    };
    crate::pkfs::read_to_string(&format!(
        ".gm/dream-rsi/{session_id}/{CYCLE_RESULT_FILE}.json"
    ))
    .and_then(|raw| serde_json::from_str(&raw).ok())
    .unwrap_or(Value::Null)
}

#[cfg(target_arch = "wasm32")]
const AUTOMATIC_REPLAY_SAMPLE: usize = 8;

#[cfg(target_arch = "wasm32")]
const SELECTION_REPLAY_RECORDED_SUCCESSES_FIRST: &str = "replay-recorded-successes-first";

#[cfg(target_arch = "wasm32")]
const SELECTION_CONTINUE_CURRENT_EXPLORATION: &str = "continue-current-exploration";

#[cfg(target_arch = "wasm32")]
fn propose_strategy() -> Result<Value, String> {
    let owner = session_id()?;
    if owner.len() > 256
        || matches!(owner.as_str(), "." | "..")
        || !owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("dream-strategy-propose requires a safe session ID".to_string());
    }
    let replay = automatic_replay(Some(owner.as_str()));
    let proposed_selection = replay
        .get("selection")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| SELECTION_CONTINUE_CURRENT_EXPLORATION.to_string());
    let path = format!(".gm/dream-rsi/{owner}/proposed-strategy.json");
    let proposal = json!({
        "ok": true,
        "kind": "strategy-proposal",
        "owner_session_id": owner,
        "proposed_selection": proposed_selection,
        "replay": replay,
        "path": &path,
    });
    if !crate::pkfs::write(&path, &proposal.to_string()) {
        return Err("dream-strategy-propose could not persist proposal".to_string());
    }
    Ok(proposal)
}

#[cfg(target_arch = "wasm32")]
pub(super) fn handle_strategy_propose(_content: &str) -> (String, String, i32) {
    match propose_strategy() {
        Ok(result) => (result.to_string(), String::new(), 0),
        Err(error) => (
            json!({ "ok": false, "error": error }).to_string(),
            String::new(),
            1,
        ),
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn handle_strategy_propose(_content: &str) -> (String, String, i32) {
    (
        json!({ "ok": false, "error": "dream-strategy-propose runs only in the wasm guest" }).to_string(),
        String::new(),
        1,
    )
}

#[cfg(target_arch = "wasm32")]
fn evaluator_receipt_path() -> Result<String, String> {
    session_store_path("evaluator-receipts")
}

#[cfg(target_arch = "wasm32")]
fn session_id() -> Result<String, String> {
    crate::orchestrator::state::dispatch_session_id()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| "Dream-RSI requires a gm session ID".to_string())
}

#[cfg(target_arch = "wasm32")]
fn signed_record(_kind: &str, record: Value) -> Result<Value, String> {
    Ok(record)
}

#[cfg(target_arch = "wasm32")]
fn verify_record(kind: &str, record: &Value) -> Result<Value, String> {
    let record = record.clone();
    let owner = record
        .get("owner_session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Dream-RSI {kind} record is missing owner_session_id"))?;
    if owner != session_id()? {
        return Err(format!(
            "Dream-RSI {kind} record belongs to another session"
        ));
    }
    if kind == "evaluator" {
        let dispatch_id = string_field(&record, "dispatch_id")?;
        let cwd = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
        let dispatch = crate::dispatch_ledger::lookup(&cwd, &dispatch_id)?
            .ok_or_else(|| "Dream-RSI evaluator dispatch is absent".to_string())?;
        if dispatch.get("session_id").and_then(Value::as_str) != Some(owner) {
            return Err("Dream-RSI evaluator dispatch belongs to another session".to_string());
        }
        let verb = dispatch
            .get("verb")
            .and_then(Value::as_str)
            .ok_or_else(|| "Dream-RSI evaluator dispatch lacks verb".to_string())?;
        let fingerprint = dispatch
            .get("fingerprint")
            .and_then(Value::as_str)
            .ok_or_else(|| "Dream-RSI evaluator dispatch lacks fingerprint".to_string())?;
        let exit_code = dispatch
            .get("exit_code")
            .and_then(Value::as_i64)
            .ok_or_else(|| "Dream-RSI evaluator dispatch lacks exit code".to_string())?;
        let output_bytes = dispatch.get("output_bytes").and_then(Value::as_u64);
        if record.get("target").and_then(Value::as_str)
            != Some(format!("{}:{}", verb, fingerprint).as_str())
            || record.get("evaluator_score").and_then(Value::as_f64)
                != Some(outcome_score(exit_code, false, output_bytes))
            || record.get("cost").and_then(Value::as_u64) != Some(1)
        {
            return Err("Dream-RSI evaluator metrics do not match dispatch evidence".to_string());
        }
    }
    Ok(record)
}

#[cfg(target_arch = "wasm32")]
fn register_policy(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content)
        .map_err(|error| format!("dream-policy-register requires JSON: {error}"))?;
    let policy = parse_policy(
        body.get("policy")
            .ok_or_else(|| "dream-policy-register requires policy".to_string())?,
    )?;
    let owner_session_id = session_id()?;
    let deployed = body
        .get("deployed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let max_online_rounds = body.get("max_online_rounds").map(|value| value.as_u64()
        .filter(|value| *value > 0)
        .ok_or_else(|| "dream-policy-register max_online_rounds must be a positive integer when present".to_string())).transpose()?;
    let policy_path = session_store_path("policies")?;
    let raw = crate::pkfs::read_to_string(&policy_path).unwrap_or_else(|| "[]".to_string());
    let mut policies = serde_json::from_str::<Value>(&raw)
        .map_err(|_| "dream-policy-register policy store is invalid".to_string())?
        .as_array()
        .cloned()
        .ok_or_else(|| "dream-policy-register policy store is invalid".to_string())?
        .into_iter()
        .map(|entry| verify_record("policy", &entry))
        .collect::<Result<Vec<_>, _>>()?;
    if policies.iter().any(|entry| {
        entry.get("id").and_then(Value::as_str) == Some(policy.id.as_str())
            && entry.get("owner_session_id").and_then(Value::as_str)
                == Some(owner_session_id.as_str())
    }) {
        return Err(format!(
            "dream-policy-register policy {} already exists",
            policy.id
        ));
    }
    if deployed {
        let incumbent = policies
            .iter()
            .find(|entry| {
                entry.get("owner_session_id").and_then(Value::as_str)
                    == Some(owner_session_id.as_str())
                    && entry.get("deployed").and_then(Value::as_bool) == Some(true)
            })
            .map(|entry| {
                (
                    entry.get("id").and_then(Value::as_str).map(str::to_string),
                    entry.get("deployed_at").and_then(Value::as_i64),
                )
            });
        if let Some((Some(incumbent), deployed_at)) = incumbent {
            let replay_path = session_store_path(ReplayFile::Replay.name())?;
            let proven = crate::pkfs::read_to_string(&replay_path)
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
                .is_some_and(|result| {
                    result.get("owner_session_id").and_then(Value::as_str)
                        == Some(owner_session_id.as_str())
                        && result.get("selected_policy_id").and_then(Value::as_str) == Some(policy.id.as_str())
                        && result.get("baseline_policy_id").and_then(Value::as_str) == Some(incumbent.as_str())
                        && result.get("improved").and_then(Value::as_bool) == Some(true)
                        && result.get("never_worse").and_then(Value::as_bool) == Some(true)
                        && deployed_at.is_some_and(|since| {
                            result.get("ts").and_then(Value::as_i64).is_some_and(|ts| ts >= since)
                        })
                });
            if !proven {
                return Err(format!(
                    "dream-policy-register deployed {} requires a replay-result that selects it over deployed baseline {incumbent}",
                    policy.id
                ));
            }
        }
        for entry in &mut policies {
            if entry.get("owner_session_id").and_then(Value::as_str)
                == Some(owner_session_id.as_str())
            {
                entry["deployed"] = Value::Bool(false);
            }
        }
    }
    let deployed_at = deployed.then(|| super::state::now_ms() as i64);
    let new_policy = json!({ "id": policy.id, "owner_session_id": owner_session_id, "roots": policy.roots, "max_nodes": policy.max_nodes, "deployed": deployed, "deployed_at": deployed_at, "max_online_rounds": max_online_rounds, "online_rounds": Vec::<u64>::new() });
    policies.push(new_policy);
    let signed = policies
        .into_iter()
        .map(|record| signed_record("policy", record))
        .collect::<Result<Vec<_>, _>>()?;
    if !crate::pkfs::write(&policy_path, &Value::Array(signed).to_string()) {
        return Err("dream-policy-register could not persist policy".to_string());
    }
    Ok(json!({ "ok": true, "policy_id": policy.id, "deployed": deployed }))
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
fn evaluator_receipt(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content)
        .map_err(|error| format!("dream-evaluator-receipt requires JSON: {error}"))?;
    let receipt_id = string_field(&body, "receipt_id")?;
    let policy_id = string_field(&body, "policy_id")?;
    let dispatch_id = string_field(&body, "dispatch_id")?;
    let parent_id = body
        .get("parent_id")
        .map(|_| string_field(&body, "parent_id"))
        .transpose()?;
    let owner_session_id = session_id()?;
    let cwd = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
    let dispatch = crate::dispatch_ledger::lookup(&cwd, &dispatch_id)?.ok_or_else(|| {
        "dream-evaluator-receipt dispatch_id is not a completed GM dispatch".to_string()
    })?;
    if dispatch.get("session_id").and_then(Value::as_str) != Some(owner_session_id.as_str()) {
        return Err("dream-evaluator-receipt dispatch belongs to another session".to_string());
    }
    let verb = dispatch
        .get("verb")
        .and_then(Value::as_str)
        .ok_or_else(|| "dream-evaluator-receipt dispatch lacks verb".to_string())?;
    let fingerprint = dispatch
        .get("fingerprint")
        .and_then(Value::as_str)
        .ok_or_else(|| "dream-evaluator-receipt dispatch lacks fingerprint".to_string())?;
    let exit_code = dispatch
        .get("exit_code")
        .and_then(Value::as_i64)
        .ok_or_else(|| "dream-evaluator-receipt dispatch lacks exit code".to_string())?;
    let output_bytes = dispatch.get("output_bytes").and_then(Value::as_u64);
    let target = format!("{}:{}", verb, fingerprint);
    let signed = signed_record(
        "evaluator",
        json!({ "id": receipt_id, "target": target, "policy_id": policy_id, "dispatch_id": dispatch_id, "evaluator_score": outcome_score(exit_code, false, output_bytes), "cost": 1, "parent_id": parent_id, "owner_session_id": owner_session_id, "metric": "completed-dispatch-success" }),
    )?;
    let path = evaluator_receipt_path()?;
    let raw = crate::pkfs::read_to_string(&path).unwrap_or_else(|| "[]".to_string());
    let mut receipts = serde_json::from_str::<Value>(&raw)
        .map_err(|_| "dream-evaluator-receipt store is invalid".to_string())?
        .as_array()
        .cloned()
        .ok_or_else(|| "dream-evaluator-receipt store is invalid".to_string())?;
    receipts.push(signed.clone());
    if !crate::pkfs::write(&path, &Value::Array(receipts).to_string()) {
        return Err("dream-evaluator-receipt could not persist receipt".to_string());
    }
    Ok(signed)
}

#[cfg(target_arch = "wasm32")]
fn record_discovery(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content)
        .map_err(|error| format!("dream-discovery-record requires JSON: {error}"))?;
    let id = string_field(&body, "id")?;
    let owner_session_id = session_id()?;
    let receipt_id = string_field(&body, "evaluator_receipt_id")?;
    let round = body
        .get("round")
        .map(|_| {
            body.get("round").and_then(Value::as_u64).ok_or_else(|| {
                "dream-discovery-record round must be a non-negative integer when present"
                    .to_string()
            })
        })
        .transpose()?;
    let receipt_path = evaluator_receipt_path()?;
    let raw_receipts = crate::pkfs::read_to_string(&receipt_path)
        .ok_or_else(|| "dream-discovery-record has no evaluator receipts".to_string())?;
    let receipts = serde_json::from_str::<Value>(&raw_receipts)
        .map_err(|_| "dream-discovery-record evaluator receipt store is invalid".to_string())?
        .as_array()
        .cloned()
        .ok_or_else(|| "dream-discovery-record evaluator receipt store is invalid".to_string())?;
    let signed_receipt = receipts
        .iter()
        .find(|receipt| receipt.get("id").and_then(Value::as_str) == Some(receipt_id.as_str()))
        .ok_or_else(|| "dream-discovery-record evaluator receipt is absent".to_string())?;
    let evaluator = verify_record("evaluator", signed_receipt)?;
    let target = string_field(&evaluator, "target")?;
    let policy_id = string_field(&evaluator, "policy_id")?;
    let dispatch_id = string_field(&evaluator, "dispatch_id")?;
    let evaluator_score = evaluator
        .get("evaluator_score")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .ok_or_else(|| {
            "dream-discovery-record evaluator receipt requires finite evaluator_score".to_string()
        })?;
    let cost = evaluator
        .get("cost")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            "dream-discovery-record evaluator receipt requires non-negative integer cost"
                .to_string()
        })?;
    let parent_id = evaluator
        .get("parent_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned);
    let policy_path = session_store_path("policies")?;
    let raw_policies = crate::pkfs::read_to_string(&policy_path)
        .ok_or_else(|| "dream-discovery-record has no registered policies".to_string())?;
    let mut policies = serde_json::from_str::<Value>(&raw_policies)
        .map_err(|_| "dream-discovery-record policy store is invalid".to_string())?
        .as_array()
        .cloned()
        .ok_or_else(|| "dream-discovery-record policy store is invalid".to_string())?
        .into_iter()
        .map(|entry| verify_record("policy", &entry))
        .collect::<Result<Vec<_>, _>>()?;
    let policy_index = policies
        .iter()
        .position(|policy| {
            policy.get("id").and_then(Value::as_str) == Some(policy_id.as_str())
                && policy.get("owner_session_id").and_then(Value::as_str)
                    == Some(owner_session_id.as_str())
        })
        .ok_or_else(|| {
            "dream-discovery-record policy_id is not registered in this session".to_string()
        })?;
    let mut policies_dirty = false;
    if let Some(max_online_rounds) = policies[policy_index]
        .get("max_online_rounds")
        .and_then(Value::as_u64)
    {
        let round = round.ok_or_else(|| format!("dream-discovery-record policy {policy_id} caps online rounds at {max_online_rounds}; round is required"))?;
        let mut online_rounds: Vec<u64> = policies[policy_index]
            .get("online_rounds")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_u64).collect())
            .unwrap_or_default();
        if !online_rounds.contains(&round) {
            if online_rounds.len() as u64 >= max_online_rounds {
                return Err(format!("dream-discovery-record policy {policy_id} has used all {max_online_rounds} online rounds; seal the recorded discoveries and dream-replay/dream-replay-round before continuing this policy's online rollout"));
            }
            online_rounds.push(round);
            policies[policy_index]["online_rounds"] =
                Value::Array(online_rounds.into_iter().map(Value::from).collect());
            policies_dirty = true;
        }
    }
    let cwd = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
    let dispatch = crate::dispatch_ledger::lookup(&cwd, &dispatch_id)?.ok_or_else(|| {
        "dream-discovery-record dispatch_id is not a completed gm dispatch".to_string()
    })?;
    if dispatch.get("exit_code").and_then(Value::as_i64) != Some(0) {
        return Err("dream-discovery-record requires a successful completed dispatch".to_string());
    }
    let discovery_path = session_store_path("discovery")?;
    let raw = crate::pkfs::read_to_string(&discovery_path).unwrap_or_else(|| "[]".to_string());
    let mut records = serde_json::from_str::<Value>(&raw)
        .map_err(|_| "dream-discovery-record store is invalid".to_string())?
        .as_array()
        .cloned()
        .ok_or_else(|| "dream-discovery-record store is invalid".to_string())?
        .into_iter()
        .map(|entry| verify_record("discovery", &entry))
        .collect::<Result<Vec<_>, _>>()?;
    if records
        .iter()
        .any(|record| record.get("id").and_then(Value::as_str) == Some(id.as_str()))
    {
        return Err(format!("dream-discovery-record {id} already exists"));
    }
    if let Some(parent_id) = &parent_id {
        if !records.iter().any(|record| {
            record.get("id").and_then(Value::as_str) == Some(parent_id.as_str())
                && record.get("owner_session_id").and_then(Value::as_str)
                    == Some(owner_session_id.as_str())
        }) {
            return Err(format!(
                "dream-discovery-record parent {parent_id} is absent"
            ));
        }
    }
    let record = json!({ "id": id, "evaluator_receipt_id": receipt_id, "owner_session_id": owner_session_id, "target": target, "policy_id": policy_id, "dispatch_id": dispatch_id, "evaluator_score": evaluator_score, "cost": cost, "round": round, "parent_id": parent_id });
    records.push(record.clone());
    let signed = records
        .into_iter()
        .map(|record| signed_record("discovery", record))
        .collect::<Result<Vec<_>, _>>()?;
    if !crate::pkfs::write(&discovery_path, &Value::Array(signed).to_string()) {
        return Err("dream-discovery-record could not persist discovery record".to_string());
    }
    if policies_dirty {
        let signed_policies = policies
            .into_iter()
            .map(|record| signed_record("policy", record))
            .collect::<Result<Vec<_>, _>>()?;
        if !crate::pkfs::write(&policy_path, &Value::Array(signed_policies).to_string()) {
            return Err(
                "dream-discovery-record could not persist policy online-round usage".to_string(),
            );
        }
    }
    Ok(json!({ "ok": true, "discovery_id": id }))
}

#[cfg(target_arch = "wasm32")]
fn seal(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content)
        .map_err(|error| format!("dream-world-seal requires JSON: {error}"))?;
    let world_id = string_field(&body, "world_id")?;
    let owner_session_id = session_id()?;
    let discovery_ids = array_field(&body, "discovery_ids")?;
    if discovery_ids.is_empty() {
        return Err("dream-world-seal requires recorded discovery_ids".to_string());
    }
    let discovery_path = session_store_path("discovery")?;
    let raw_records = crate::pkfs::read_to_string(&discovery_path)
        .ok_or_else(|| "dream-world-seal has no discovery records".to_string())?;
    let records = serde_json::from_str::<Value>(&raw_records)
        .map_err(|_| "dream-world-seal discovery record store is invalid".to_string())?
        .as_array()
        .cloned()
        .ok_or_else(|| "dream-world-seal discovery record store is invalid".to_string())?
        .into_iter()
        .map(|entry| verify_record("discovery", &entry))
        .collect::<Result<Vec<_>, _>>()?;
    let nodes = discovery_ids.iter().enumerate().map(|(index, value)| {
        let id = value.as_str().ok_or_else(|| "dream-world-seal discovery_ids must contain strings".to_string())?;
        let record = records.iter().find(|record| record.get("id").and_then(Value::as_str) == Some(id)).ok_or_else(|| format!("dream-world-seal discovery {id} is absent"))?;
        if record.get("owner_session_id").and_then(Value::as_str) != Some(owner_session_id.as_str()) { return Err(format!("dream-world-seal discovery {id} belongs to another session")); }
        let receipt_id = string_field(record, "evaluator_receipt_id")?;
        let receipt_path = evaluator_receipt_path()?;
        let raw_receipts = crate::pkfs::read_to_string(&receipt_path).ok_or_else(|| "dream-world-seal has no evaluator receipts".to_string())?;
        let receipts = serde_json::from_str::<Value>(&raw_receipts).map_err(|_| "dream-world-seal evaluator receipt store is invalid".to_string())?.as_array().cloned().ok_or_else(|| "dream-world-seal evaluator receipt store is invalid".to_string())?;
        let receipt = receipts.iter().find(|receipt| receipt.get("id").and_then(Value::as_str) == Some(receipt_id.as_str())).ok_or_else(|| format!("dream-world-seal evaluator receipt {receipt_id} is absent"))?;
        let evaluator = verify_record("evaluator", receipt)?;
        if evaluator.get("policy_id").and_then(Value::as_str) != record.get("policy_id").and_then(Value::as_str)
            || evaluator.get("dispatch_id").and_then(Value::as_str) != record.get("dispatch_id").and_then(Value::as_str) { return Err(format!("dream-world-seal discovery {id} does not match evaluator receipt")); }
        let score = evaluator.get("evaluator_score").and_then(Value::as_f64).ok_or_else(|| format!("dream-world-seal evaluator receipt {receipt_id} lacks score"))?;
        let cost = evaluator.get("cost").and_then(Value::as_u64).ok_or_else(|| format!("dream-world-seal evaluator receipt {receipt_id} lacks cost"))?;
        let round = record.get("round").and_then(Value::as_u64).unwrap_or(index as u64);
        let children = records.iter()
            .filter(|other| other.get("parent_id").and_then(Value::as_str) == Some(id)
                && discovery_ids.iter().any(|listed| listed.as_str() == Some(other.get("id").and_then(Value::as_str).unwrap_or(""))))
            .filter_map(|other| other.get("id").and_then(Value::as_str))
            .collect::<Vec<_>>();
        Ok(json!({ "id": id, "score": score, "cost": cost, "round": round, "children": children }))
    }).collect::<Result<Vec<_>, String>>()?;
    let world = parse_world(&json!({ "id": world_id, "nodes": nodes }))?;
    let world_path = session_store_path("worlds")?;
    let raw = crate::pkfs::read_to_string(&world_path).unwrap_or_else(|| "[]".to_string());
    let mut worlds = serde_json::from_str::<Value>(&raw)
        .map_err(|_| "dream-world-seal world store is invalid".to_string())?
        .as_array()
        .cloned()
        .ok_or_else(|| "dream-world-seal world store is invalid".to_string())?
        .into_iter()
        .map(|entry| verify_record("world", &entry))
        .collect::<Result<Vec<_>, _>>()?;
    if worlds
        .iter()
        .any(|entry| entry.get("id").and_then(Value::as_str) == Some(world.id.as_str()))
    {
        return Err(format!(
            "dream-world-seal world {} already exists",
            world.id
        ));
    }
    worlds.push(json!({ "id": world.id, "owner_session_id": owner_session_id, "discovery_ids": discovery_ids, "nodes": nodes }));
    let signed = worlds
        .into_iter()
        .map(|record| signed_record("world", record))
        .collect::<Result<Vec<_>, _>>()?;
    if !crate::pkfs::write(&world_path, &Value::Array(signed).to_string()) {
        return Err("dream-world-seal could not persist sealed world".to_string());
    }
    Ok(json!({ "ok": true, "world_id": world.id, "node_count": world.nodes.len() }))
}

#[cfg(target_arch = "wasm32")]
fn replay_rounds_path() -> Result<String, String> {
    session_store_path("replay-rounds")
}

#[cfg(target_arch = "wasm32")]
fn eligible_nodes(
    roots: &[String],
    revealed: &BTreeSet<String>,
    world: &World,
) -> BTreeSet<String> {
    roots
        .iter()
        .cloned()
        .chain(
            revealed
                .iter()
                .filter(|id| {
                    world
                        .nodes
                        .get(id.as_str())
                        .map(|node| !node.children.iter().any(|child| revealed.contains(child)))
                        .unwrap_or(false)
                })
                .cloned(),
        )
        .filter(|id| world.nodes.contains_key(id))
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn replay_round(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content)
        .map_err(|error| format!("dream-replay-round requires JSON: {error}"))?;
    let owner_session_id = session_id()?;
    let replay_id = string_field(&body, "replay_id")?;
    let path = replay_rounds_path()?;
    let raw = crate::pkfs::read_to_string(&path).unwrap_or_else(|| "[]".to_string());
    let mut states = serde_json::from_str::<Value>(&raw)
        .map_err(|_| "dream-replay-round state store is invalid".to_string())?
        .as_array()
        .cloned()
        .ok_or_else(|| "dream-replay-round state store is invalid".to_string())?;
    let existing_index = states.iter().position(|state| {
        state.get("replay_id").and_then(Value::as_str) == Some(replay_id.as_str())
            && state.get("owner_session_id").and_then(Value::as_str)
                == Some(owner_session_id.as_str())
    });

    let mut state = if let Some(index) = existing_index {
        states[index].clone()
    } else {
        let policy_id = string_field(&body, "policy_id")?;
        let world_id = string_field(&body, "world_id")?;
        let max_rounds = body
            .get("max_rounds")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
            .ok_or_else(|| "dream-replay-round requires positive integer max_rounds".to_string())?;
        let beta1 = nonneg_f64_field(&body, "beta1")?;
        let beta2 = nonneg_f64_field(&body, "beta2")?;
        let round_budget = body.get("round_budget").and_then(Value::as_u64);
        let policy_path = session_store_path("policies")?;
        let raw_policies = crate::pkfs::read_to_string(&policy_path)
            .ok_or_else(|| "dream-replay-round has no registered policies".to_string())?;
        let stored_policies = serde_json::from_str::<Value>(&raw_policies)
            .map_err(|_| "dream-replay-round policy store is invalid".to_string())?
            .as_array()
            .cloned()
            .ok_or_else(|| "dream-replay-round policy store is invalid".to_string())?
            .into_iter()
            .map(|entry| verify_record("policy", &entry))
            .collect::<Result<Vec<_>, _>>()?;
        let stored_policy = stored_policies
            .iter()
            .find(|policy| {
                policy.get("id").and_then(Value::as_str) == Some(policy_id.as_str())
                    && policy.get("owner_session_id").and_then(Value::as_str)
                        == Some(owner_session_id.as_str())
            })
            .ok_or_else(|| format!("dream-replay-round registered policy {policy_id} is absent"))?;
        let policy = parse_policy(stored_policy)?;
        let world_path = session_store_path("worlds")?;
        let raw_worlds = crate::pkfs::read_to_string(&world_path)
            .ok_or_else(|| "dream-replay-round has no sealed worlds".to_string())?;
        let stored_worlds = serde_json::from_str::<Value>(&raw_worlds)
            .map_err(|_| "dream-replay-round world store is invalid".to_string())?
            .as_array()
            .cloned()
            .ok_or_else(|| "dream-replay-round world store is invalid".to_string())?
            .into_iter()
            .map(|entry| verify_record("world", &entry))
            .collect::<Result<Vec<_>, _>>()?;
        let stored_world = stored_worlds
            .iter()
            .find(|world| world.get("id").and_then(Value::as_str) == Some(world_id.as_str()))
            .ok_or_else(|| format!("dream-replay-round sealed world {world_id} is absent"))?;
        if stored_world.get("owner_session_id").and_then(Value::as_str)
            != Some(owner_session_id.as_str())
        {
            return Err(format!(
                "dream-replay-round sealed world {world_id} belongs to another session"
            ));
        }
        parse_world(stored_world)?;
        json!({
            "replay_id": replay_id,
            "owner_session_id": owner_session_id,
            "policy_id": policy_id,
            "world_id": world_id,
            "roots": policy.roots,
            "max_nodes": policy.max_nodes,
            "max_rounds": max_rounds,
            "beta1": beta1,
            "beta2": beta2,
            "round_budget": round_budget,
            "revealed": Vec::<String>::new(),
            "rounds_used": 0,
            "quality": Value::Null,
            "cost": 0,
            "closed": false,
        })
    };

    if state.get("closed").and_then(Value::as_bool) == Some(true) {
        return Err(format!("dream-replay-round {replay_id} is already closed"));
    }

    let world_id = string_field(&state, "world_id")?;
    let world_path = session_store_path("worlds")?;
    let raw_worlds = crate::pkfs::read_to_string(&world_path)
        .ok_or_else(|| "dream-replay-round has no sealed worlds".to_string())?;
    let stored_worlds = serde_json::from_str::<Value>(&raw_worlds)
        .map_err(|_| "dream-replay-round world store is invalid".to_string())?
        .as_array()
        .cloned()
        .ok_or_else(|| "dream-replay-round world store is invalid".to_string())?
        .into_iter()
        .map(|entry| verify_record("world", &entry))
        .collect::<Result<Vec<_>, _>>()?;
    let stored_world = stored_worlds
        .iter()
        .find(|world| world.get("id").and_then(Value::as_str) == Some(world_id.as_str()))
        .ok_or_else(|| format!("dream-replay-round sealed world {world_id} is absent"))?;
    let world = parse_world(stored_world)?;

    let roots: Vec<String> = state
        .get("roots")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let max_nodes = state
        .get("max_nodes")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX) as usize;
    let max_rounds = state.get("max_rounds").and_then(Value::as_u64).unwrap_or(0);
    let beta1 = state.get("beta1").and_then(Value::as_f64).unwrap_or(0.0);
    let beta2 = state.get("beta2").and_then(Value::as_f64).unwrap_or(0.0);
    let mut revealed: BTreeSet<String> = state
        .get("revealed")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let mut rounds_used = state
        .get("rounds_used")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut quality = state
        .get("quality")
        .and_then(Value::as_f64)
        .unwrap_or(f64::NEG_INFINITY);
    let mut cost = state.get("cost").and_then(Value::as_u64).unwrap_or(0);
    let round_budget = state.get("round_budget").and_then(Value::as_u64);

    let eligible = eligible_nodes(&roots, &revealed, &world);
    let requested_batch: Vec<String> = body
        .get("batch")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let finalize =
        requested_batch.is_empty() || rounds_used >= max_rounds || revealed.len() >= max_nodes;

    let mut newly_revealed = Vec::new();
    if !finalize {
        for id in &requested_batch {
            if !eligible.contains(id) {
                return Err(format!(
                    "dream-replay-round {id} is not an eligible continuation this round"
                ));
            }
        }
        let mut round_cost = 0_u64;
        for id in &requested_batch {
            let node = world
                .nodes
                .get(id)
                .ok_or_else(|| format!("dream-replay-round world {world_id} has no node {id}"))?;
            for child_id in &node.children {
                if revealed.len() >= max_nodes {
                    break;
                }
                if revealed.insert(child_id.clone()) {
                    let child = world.nodes.get(child_id).ok_or_else(|| format!("dream-replay-round world {world_id} node {id} references unobserved child {child_id}"))?;
                    quality = quality.max(child.score);
                    cost = cost
                        .checked_add(child.cost)
                        .ok_or_else(|| "dream-replay-round cost overflow".to_string())?;
                    round_cost = round_cost
                        .checked_add(child.cost)
                        .ok_or_else(|| "dream-replay-round round cost overflow".to_string())?;
                    newly_revealed
                        .push(json!({ "id": child_id, "score": child.score, "cost": child.cost }));
                }
            }
        }
        rounds_used += 1;
    }

    let closed = finalize || rounds_used >= max_rounds || revealed.len() >= max_nodes;
    state["revealed"] = Value::Array(revealed.iter().cloned().map(Value::String).collect());
    state["rounds_used"] = Value::from(rounds_used);
    state["quality"] = if quality.is_finite() {
        Value::from(quality)
    } else {
        Value::Null
    };
    state["cost"] = Value::from(cost);
    state["closed"] = Value::Bool(closed);

    let response = if closed {
        let attempts = revealed.len() as f64;
        let parallelism_bonus = if rounds_used > 0 {
            attempts / rounds_used as f64
        } else {
            0.0
        };
        let final_quality = if quality.is_finite() { quality } else { 0.0 };
        let replay_score = final_quality - beta1 * cost as f64 + beta2 * parallelism_bonus;
        state["replay_score"] = Value::from(replay_score);
        json!({
            "ok": true, "replay_id": replay_id, "closed": true, "rounds_used": rounds_used,
            "revealed_count": revealed.len(), "quality": final_quality, "cost": cost,
            "parallelism_bonus": parallelism_bonus, "replay_score": replay_score,
            "round_budget": round_budget,
        })
    } else {
        json!({
            "ok": true, "replay_id": replay_id, "closed": false, "rounds_used": rounds_used,
            "rounds_remaining": max_rounds.saturating_sub(rounds_used), "revealed_now": newly_revealed,
            "eligible_next": eligible_nodes(&roots, &revealed, &world).into_iter().collect::<Vec<_>>(),
            "quality_so_far": if quality.is_finite() { Value::from(quality) } else { Value::Null },
            "cost_so_far": cost,
            "round_budget": round_budget,
        })
    };

    if let Some(index) = existing_index {
        states[index] = state;
    } else {
        states.push(state);
    }
    if !crate::pkfs::write(&path, &Value::Array(states).to_string()) {
        return Err("dream-replay-round could not persist state".to_string());
    }
    Ok(response)
}

#[cfg(target_arch = "wasm32")]
pub(super) fn handle_replay_round(content: &str) -> (String, String, i32) {
    match replay_round(content) {
        Ok(result) => (result.to_string(), String::new(), 0),
        Err(error) => (
            json!({ "ok": false, "error": error }).to_string(),
            String::new(),
            1,
        ),
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn handle_dream_round(content: &str) -> (String, String, i32) {
    match dream_round(content) {
        Ok(result) => (result.to_string(), String::new(), 0),
        Err(error) => (
            json!({ "ok": false, "error": error }).to_string(),
            String::new(),
            1,
        ),
    }
}

#[cfg(target_arch = "wasm32")]
fn evaluate(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content)
        .map_err(|error| format!("dream-replay requires JSON: {error}"))?;
    let owner_session_id = session_id()?;
    if body.get("worlds").is_some() {
        return Err(
            "dream-replay accepts sealed world_ids, never caller-supplied worlds".to_string(),
        );
    }
    let ids = array_field(&body, "world_ids")?;
    let baseline_id = string_field(&body, "baseline_policy_id")?;
    let policy_ids = array_field(&body, "policy_ids")?;
    let beta1 = nonneg_f64_field(&body, "beta1")?;
    let beta2 = nonneg_f64_field(&body, "beta2")?;
    let policy_path = session_store_path("policies")?;
    let raw_policies = crate::pkfs::read_to_string(&policy_path)
        .ok_or_else(|| "dream-replay has no registered policies".to_string())?;
    let stored_policies = serde_json::from_str::<Value>(&raw_policies)
        .map_err(|_| "dream-replay policy store is invalid".to_string())?
        .as_array()
        .cloned()
        .ok_or_else(|| "dream-replay policy store is invalid".to_string())?
        .into_iter()
        .map(|entry| verify_record("policy", &entry))
        .collect::<Result<Vec<_>, _>>()?;
    let policies = policy_ids
        .iter()
        .map(|id| {
            let id = id
                .as_str()
                .ok_or_else(|| "dream-replay policy_ids must contain strings".to_string())?;
            let stored_policy = stored_policies
                .iter()
                .find(|policy| {
                    policy.get("id").and_then(Value::as_str) == Some(id)
                        && policy.get("owner_session_id").and_then(Value::as_str)
                            == Some(owner_session_id.as_str())
                })
                .ok_or_else(|| format!("dream-replay registered policy {id} is absent"))?;
            parse_policy(stored_policy)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !stored_policies.iter().any(|policy| {
        policy.get("id").and_then(Value::as_str) == Some(baseline_id.as_str())
            && policy.get("owner_session_id").and_then(Value::as_str)
                == Some(owner_session_id.as_str())
            && policy.get("deployed").and_then(Value::as_bool) == Some(true)
    }) {
        return Err(
            "dream-replay baseline_policy_id is not this session's deployed policy".to_string(),
        );
    }
    let world_path = session_store_path("worlds")?;
    let raw = crate::pkfs::read_to_string(&world_path)
        .ok_or_else(|| "dream-replay has no sealed worlds".to_string())?;
    let stored = serde_json::from_str::<Value>(&raw)
        .map_err(|_| "dream-replay sealed world store is invalid".to_string())?
        .as_array()
        .cloned()
        .ok_or_else(|| "dream-replay sealed world store is invalid".to_string())?
        .into_iter()
        .map(|entry| verify_record("world", &entry))
        .collect::<Result<Vec<_>, _>>()?;
    let worlds = ids
        .iter()
        .map(|id| {
            let id = id
                .as_str()
                .ok_or_else(|| "dream-replay world_ids must contain strings".to_string())?;
            let stored_world = stored
                .iter()
                .find(|world| world.get("id").and_then(Value::as_str) == Some(id))
                .ok_or_else(|| format!("dream-replay sealed world {id} is absent"))?;
            if stored_world.get("owner_session_id").and_then(Value::as_str)
                != Some(owner_session_id.as_str())
            {
                return Err(format!(
                    "dream-replay sealed world {id} belongs to another session"
                ));
            }
            parse_world(stored_world)
        })
        .collect::<Result<Vec<_>, _>>()?;
    evaluate_worlds(baseline_id, policies, worlds, beta1, beta2)
}

#[cfg(target_arch = "wasm32")]
fn dream_round(content: &str) -> Result<Value, String> {
    let mut body: Value = serde_json::from_str(content)
        .map_err(|error| format!("dream-round requires JSON: {error}"))?;
    let baseline_id = string_field(&body, "baseline_policy_id")?;
    let mut policy_ids = vec![Value::String(baseline_id.clone())];
    for candidate in array_field(&body, "candidate_policy_ids")? {
        if !policy_ids.contains(candidate) {
            policy_ids.push(candidate.clone());
        }
    }
    body["policy_ids"] = Value::Array(policy_ids);
    let mut result = evaluate(&body.to_string())?;
    let mut rankings = result
        .get("rankings")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "dream-round ranking missing".to_string())?;
    rankings.sort_by(|left, right| {
        right
            .get("score")
            .and_then(Value::as_f64)
            .partial_cmp(&left.get("score").and_then(Value::as_f64))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    result["rankings"] = Value::Array(rankings);
    persist_sealed_replay("dream-round", &mut result);
    Ok(result)
}

#[cfg(not(target_arch = "wasm32"))]
fn evaluate(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content)
        .map_err(|error| format!("dream-replay requires JSON: {error}"))?;
    let baseline_id = string_field(&body, "baseline_policy_id")?;
    let policies = array_field(&body, "policies")?
        .iter()
        .map(parse_policy)
        .collect::<Result<Vec<_>, _>>()?;
    let worlds = array_field(&body, "worlds")?
        .iter()
        .map(parse_world)
        .collect::<Result<Vec<_>, _>>()?;
    let beta1 = nonneg_f64_field(&body, "beta1")?;
    let beta2 = nonneg_f64_field(&body, "beta2")?;
    evaluate_worlds(baseline_id, policies, worlds, beta1, beta2)
}

#[cfg(target_arch = "wasm32")]
pub(super) fn handle_policy_register(content: &str) -> (String, String, i32) {
    match register_policy(content) {
        Ok(result) => (result.to_string(), String::new(), 0),
        Err(error) => (
            json!({ "ok": false, "error": error }).to_string(),
            String::new(),
            1,
        ),
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn handle_evaluator_receipt(content: &str) -> (String, String, i32) {
    match evaluator_receipt(content) {
        Ok(result) => (result.to_string(), String::new(), 0),
        Err(error) => (
            json!({ "ok": false, "error": error }).to_string(),
            String::new(),
            1,
        ),
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn handle_discovery_record(content: &str) -> (String, String, i32) {
    match record_discovery(content) {
        Ok(result) => (result.to_string(), String::new(), 0),
        Err(error) => (
            json!({ "ok": false, "error": error }).to_string(),
            String::new(),
            1,
        ),
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn handle_seal(content: &str) -> (String, String, i32) {
    match seal(content) {
        Ok(result) => (result.to_string(), String::new(), 0),
        Err(error) => (
            json!({ "ok": false, "error": error }).to_string(),
            String::new(),
            1,
        ),
    }
}

#[cfg(target_arch = "wasm32")]
pub const ACTIVE_STRATEGY_FILE: &str = "active-strategy";

#[cfg(target_arch = "wasm32")]
pub const REPLAY_RESULT_FILE: &str = "replay-result";

#[cfg(target_arch = "wasm32")]
pub const CYCLE_RESULT_FILE: &str = "cycle-result";

#[cfg(target_arch = "wasm32")]
pub const INDEX_PATH: &str = ".gm/dream-rsi/index.json";

#[cfg(target_arch = "wasm32")]
pub const INDEX_SCHEMA_VERSION: u32 = 1;

#[cfg(target_arch = "wasm32")]
fn write_index() {
    let index = json!({
        "schema_version": INDEX_SCHEMA_VERSION,
        "files": {
            "observations": "observations.json",
            "active_strategy": format!("{ACTIVE_STRATEGY_FILE}.json"),
            "replay_result": format!("{REPLAY_RESULT_FILE}.json"),
            "cycle_result": format!("{CYCLE_RESULT_FILE}.json"),
        },
        "record_kinds": {
            "replay_result": "replay-result",
            "cycle_result": "cycle-result",
        },
        "fields": ["kind", "source", "owner_session_id", "ts"],
    });
    let _ = crate::pkfs::write(INDEX_PATH, &index.to_string());
}

#[cfg(target_arch = "wasm32")]
pub enum ReplayFile {
    Cycle,
    Replay,
}

#[cfg(target_arch = "wasm32")]
impl ReplayFile {
    fn name(&self) -> &'static str {
        match self {
            ReplayFile::Cycle => CYCLE_RESULT_FILE,
            ReplayFile::Replay => REPLAY_RESULT_FILE,
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
    let path = session_store_path(name)?;
    let kind = match file {
        ReplayFile::Cycle => "cycle-result",
        ReplayFile::Replay => "replay-result",
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

#[cfg(target_arch = "wasm32")]
fn persist_sealed_replay(source: &'static str, replay: &mut Value) {
    let baseline_score = replay.get("baseline_score").and_then(Value::as_f64);
    let selected_score = replay.get("selected_score").and_then(Value::as_f64);
    let rankings = replay
        .get("rankings")
        .and_then(Value::as_array)
        .map(|rankings| {
            rankings
                .iter()
                .map(|ranking| json!({
                    "policy_id": ranking.get("policy_id"),
                    "score": ranking.get("score"),
                    "cost": ranking.get("cost"),
                }))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let ranked_score = |id: Option<&str>| {
        id.and_then(|id| rankings.iter().find(|ranking| ranking.get("policy_id").and_then(Value::as_str) == Some(id)))
            .and_then(|ranking| ranking.get("score").and_then(Value::as_f64))
    };
    let never_worse = match (
        ranked_score(replay.get("selected_policy_id").and_then(Value::as_str)),
        ranked_score(replay.get("baseline_policy_id").and_then(Value::as_str)),
    ) {
        (Some(selected), Some(baseline)) => selected >= baseline,
        _ => false,
    };
    let summary = json!({
        "baseline_policy_id": replay.get("baseline_policy_id"),
        "selected_policy_id": replay.get("selected_policy_id"),
        "baseline_score": baseline_score,
        "selected_score": selected_score,
        "n_observations": replay.get("n_observations"),
        "domain": replay.get("domain"),
        "improved": replay.get("improved"),
        "never_worse": never_worse,
        "beta1": replay.get("beta1"),
        "beta2": replay.get("beta2"),
        "rankings": rankings,
    });
    let persisted = persist_replay_result(
        ReplayFile::Replay,
        ReplaySource(source),
        summary.as_object().cloned().unwrap_or_default(),
    )
    .ok();
    replay["replay_result_path"] = persisted.map(Value::String).unwrap_or(Value::Null);
}

#[cfg(target_arch = "wasm32")]
pub(super) fn handle(content: &str) -> (String, String, i32) {
    let replay = match evaluate(content) {
        Ok(mut replay) => {
            persist_sealed_replay("dream-replay", &mut replay);
            replay
        }
        Err(error) => {
            return (
                json!({ "ok": false, "error": error }).to_string(),
                String::new(),
                1,
            )
        }
    };
    let (observations, rejected_count) = match observations_for_result() {
        Ok(found) => found,
        Err(error) => {
            return (
                json!({ "ok": false, "error": error }).to_string(),
                String::new(),
                1,
            )
        }
    };
    let scorer = match session_id() {
        Ok(scorer) => scorer,
        Err(error) => {
            return (
                json!({ "ok": false, "error": error }).to_string(),
                String::new(),
                1,
            )
        }
    };
    let walk_files = lean_walk_files();
    let mut reply = result_json(&produce_result(&observations, &replay, &scorer, &walk_files));
    reply["rejected_count"] = json!(rejected_count);
    reply["replay"] = replay;
    (reply.to_string(), String::new(), 0)
}

#[cfg(target_arch = "wasm32")]
fn observations_for_result() -> Result<(Vec<ObservationRecord>, usize), String> {
    let session = session_id()?;
    let path = format!(".gm/dream-rsi/{session}/observations.json");
    let raw = recover_observations_tail(&path).unwrap_or_default();
    Ok(read_observations(&raw))
}

#[cfg(target_arch = "wasm32")]
fn per_verb_stats(observations: &[ObservationRecord]) -> Vec<VerbStat> {
    let mut tallies: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
    for record in observations {
        let verb = record.verb.clone().unwrap_or_else(|| "unknown".to_owned());
        let tally = tallies.entry(verb).or_insert((0, 0, 0));
        tally.0 += 1;
        if !record.succeeded() && !record.no_match {
            tally.1 += 1;
        }
        if record.is_gate_drift_failure() {
            tally.2 += 1;
        }
    }
    tallies
        .into_iter()
        .map(|(verb, (count, failures, drifts))| VerbStat {
            verb,
            count,
            failure_rate: failures as f64 / count as f64,
            gate_drift_rate: drifts as f64 / count as f64,
        })
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn replay_proposals(replay: &Value) -> Vec<VerbProposal> {
    let rankings = replay
        .get("rankings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some(baseline) = rankings.first() else {
        return Vec::new();
    };
    let baseline_id = baseline
        .get("policy_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let baseline_score = baseline.get("score").and_then(Value::as_f64).unwrap_or(0.0);
    rankings
        .iter()
        .skip(1)
        .filter_map(|ranking| {
            let policy_id = ranking.get("policy_id").and_then(Value::as_str)?;
            let score = ranking.get("score").and_then(Value::as_f64)?;
            (score > baseline_score).then(|| VerbProposal {
                verb: policy_id.to_owned(),
                change: format!("replace baseline {baseline_id}"),
                evidence: format!("replay score {score:.6} above baseline {baseline_score:.6}"),
                target: "dream-replay".to_owned(),
            })
        })
        .take(MAX_PART_A_PROPOSALS)
        .collect()
}

#[cfg(target_arch = "wasm32")]
struct LeanWalkFile {
    node: String,
    session: String,
    verified_by: Option<String>,
}

#[cfg(target_arch = "wasm32")]
fn lean_walk_files() -> Vec<LeanWalkFile> {
    dir_entry_names(".gm/lean-walk")
        .into_iter()
        .filter_map(|name| {
            let stem = name.strip_suffix(".json")?;
            let (file_node, file_session) = stem.split_once('.')?;
            let record = crate::pkfs::read_to_string(&format!(".gm/lean-walk/{name}"))
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
            let field = |key: &str| {
                record
                    .as_ref()
                    .and_then(|value| value.get(key))
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_owned)
            };
            Some(LeanWalkFile {
                node: field("node").unwrap_or_else(|| file_node.to_owned()),
                session: field("session").unwrap_or_else(|| file_session.to_owned()),
                verified_by: field("verified_by"),
            })
        })
        .collect()
}

#[cfg(target_arch = "wasm32")]
fn derive_part_b(
    observations: &[ObservationRecord],
    scorer: &str,
    walk_files: &[LeanWalkFile],
) -> PartB {
    let self_scored: BTreeSet<&str> = walk_files
        .iter()
        .filter(|walk| {
            walk.session == scorer
                || walk
                    .verified_by
                    .as_deref()
                    .map_or(false, |verifier| verifier == walk.session.as_str())
        })
        .map(|walk| walk.node.as_str())
        .collect();
    let mut walks: BTreeMap<String, (bool, Vec<String>)> = BTreeMap::new();
    for record in observations {
        let Some(node) = record.lean_node.as_deref() else {
            continue;
        };
        let walk = walks.entry(node.to_owned()).or_insert((false, Vec::new()));
        match record.dispatch_id.as_deref() {
            Some(_) if record.succeeded() => walk.0 = true,
            Some(dispatch_id) => walk.1.push(dispatch_id.to_owned()),
            None => {}
        }
    }
    let traversable_nodes = walks.len() as u64;
    let nodes_verified = walks
        .iter()
        .filter(|(node, (verified, _))| *verified && !self_scored.contains(node.as_str()))
        .count() as u64;
    let mut rejected: BTreeMap<String, RejectedWalk> = walks
        .iter()
        .filter(|(node, (verified, _))| !*verified || self_scored.contains(node.as_str()))
        .map(|(node, (_, missing))| {
            let reason = self_scored.contains(node.as_str()).then_some("self_scored");
            (
                node.clone(),
                RejectedWalk {
                    node: node.clone(),
                    missing_dispatch_ids: missing.clone(),
                    reason,
                },
            )
        })
        .collect();
    for node in &self_scored {
        rejected.entry((*node).to_owned()).or_insert_with(|| RejectedWalk {
            node: (*node).to_owned(),
            missing_dispatch_ids: Vec::new(),
            reason: Some("self_scored"),
        });
    }
    let rejected_walks = rejected.into_values().collect();
    PartB {
        traversable_nodes,
        nodes_verified,
        backreference_edges: 0,
        backreferences_verified: 0,
        forward_edge_coverage_approx: if traversable_nodes == 0 {
            0.0
        } else {
            nodes_verified as f64 / traversable_nodes as f64
        },
        rejected_walks,
        proposals: Vec::new(),
    }
}

#[cfg(target_arch = "wasm32")]
fn produce_result(
    observations: &[ObservationRecord],
    replay: &Value,
    scorer: &str,
    walk_files: &[LeanWalkFile],
) -> DreamRsiResult {
    let records = observations.len() as u64;
    let part_a = if records < INSUFFICIENT_RECORD_THRESHOLD {
        PartA::Insufficient { records }
    } else {
        PartA::Measured {
            records,
            per_verb: per_verb_stats(observations),
            proposals: replay_proposals(replay),
        }
    };
    DreamRsiResult {
        status: DreamRsiStatus::Proposed,
        part_a,
        part_b: derive_part_b(observations, scorer, walk_files),
    }
}

#[cfg(target_arch = "wasm32")]
fn result_json(result: &DreamRsiResult) -> Value {
    let part_a = match &result.part_a {
        PartA::Insufficient { records } => json!({ "state": "insufficient", "records": records }),
        PartA::Measured {
            records,
            per_verb,
            proposals,
        } => json!({
            "state": "measured",
            "records": records,
            "per_verb": per_verb
                .iter()
                .map(|stat| json!({
                    "verb": stat.verb,
                    "count": stat.count,
                    "failure_rate": stat.failure_rate,
                    "gate_drift_rate": stat.gate_drift_rate,
                }))
                .collect::<Vec<_>>(),
            "proposals": proposals
                .iter()
                .map(|proposal| json!({
                    "verb": proposal.verb,
                    "change": proposal.change,
                    "evidence": proposal.evidence,
                    "target": proposal.target,
                }))
                .collect::<Vec<_>>(),
        }),
    };
    let part_b = &result.part_b;
    json!({
        "status": "proposed",
        "part_a": part_a,
        "part_b": {
            "traversable_nodes": part_b.traversable_nodes,
            "nodes_verified": part_b.nodes_verified,
            "backreference_edges": part_b.backreference_edges,
            "backreferences_verified": part_b.backreferences_verified,
            "backreference_judgement": "model-judged",
            "forward_edge_coverage_approx": part_b.forward_edge_coverage_approx,
            "rejected_walks": part_b
                .rejected_walks
                .iter()
                .map(|walk| json!({
                    "node": walk.node,
                    "missing_dispatch_ids": walk.missing_dispatch_ids,
                    "reason": walk.reason,
                }))
                .collect::<Vec<_>>(),
            "proposals": part_b
                .proposals
                .iter()
                .map(|proposal| json!({
                    "node": proposal.node,
                    "walk_when": proposal.walk_when,
                    "evidence_that_counts": proposal.evidence_that_counts,
                }))
                .collect::<Vec<_>>(),
        },
    })
}
