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

const PROMOTION_MIN_WORLDS: usize = 3;
const PROMOTION_MIN_WIN_FRACTION: f64 = 0.5;

struct RankedReplay {
    policy_id: String,
    score: f64,
    cost: u64,
}

struct ReplayOutcome {
    baseline_score: f64,
    selected: RankedReplay,
    rankings: Vec<RankedReplay>,
}

fn finite_score(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn evaluate_worlds(
    baseline_id: String,
    mut policies: Vec<Policy>,
    worlds: Vec<World>,
    beta1: f64,
    beta2: f64,
    promotion_margin: f64,
) -> Result<ReplayOutcome, String> {
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
    // every ranking so the candidates stay comparable.
    let evaluated: Vec<Vec<Result<Value, String>>> = policies
        .iter()
        .map(|policy| worlds.iter().map(|world| evaluate_policy(world, policy, beta1, beta2)).collect())
        .collect();
    let mut usable = vec![true; worlds.len()];
    for policy_results in &evaluated {
        for (index, result) in policy_results.iter().enumerate() {
            if result.is_err() {
                usable[index] = false;
            }
        }
    }
    let usable_count = usable.iter().filter(|kept| **kept).count();
    if usable_count == 0 {
        return Err("dream-replay-cycle: no sealed world replays under every policy".to_string());
    }
    let mut ranked: Vec<RankedReplay> = Vec::new();
    let mut world_scores: Vec<Vec<f64>> = Vec::new();
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
        ranked.push(RankedReplay { policy_id: policy.id.clone(), score, cost });
        world_scores.push(scores);
    }
    let baseline_index = ranked
        .iter()
        .position(|ranking| ranking.policy_id == baseline_id)
        .ok_or_else(|| "dream-replay-cycle baseline ranking missing".to_string())?;
    let baseline_score = ranked[baseline_index].score;
    let baseline_world_scores = world_scores[baseline_index].clone();
    let win_fractions: Vec<f64> = world_scores
        .iter()
        .map(|scores| {
            let wins = scores
                .iter()
                .zip(baseline_world_scores.iter())
                .filter(|(challenger, incumbent)| challenger > incumbent)
                .count();
            wins as f64 / usable_count as f64
        })
        .collect();
    let clears_margin = |index: usize| -> bool {
        finite_score(ranked[index].score).unwrap_or(f64::NEG_INFINITY) > baseline_score + promotion_margin
    };
    let clears_win_fraction = |index: usize| -> bool { win_fractions[index] >= PROMOTION_MIN_WIN_FRACTION };
    let selected_index = (0..ranked.len())
        .filter(|&index| usable_count >= PROMOTION_MIN_WORLDS && clears_margin(index) && clears_win_fraction(index))
        .max_by(|&left, &right| {
            let score = |index: usize| finite_score(ranked[index].score).unwrap_or(f64::NEG_INFINITY);
            score(left)
                .partial_cmp(&score(right))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| ranked[right].cost.cmp(&ranked[left].cost))
                .then_with(|| ranked[right].policy_id.cmp(&ranked[left].policy_id))
        })
        .unwrap_or(baseline_index);
    let selected = RankedReplay {
        policy_id: ranked[selected_index].policy_id.clone(),
        score: ranked[selected_index].score,
        cost: ranked[selected_index].cost,
    };
    Ok(ReplayOutcome { baseline_score, selected, rankings: ranked })
}

pub struct ReplayScores {
    pub baseline: Option<f64>,
    pub selected_policy_id: String,
    pub selected: Option<f64>,
    pub ranked: Vec<(String, f64)>,
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

pub const OBSERVATIONS_FILE: &str = "observations.json";


#[cfg(target_arch = "wasm32")]
mod history;
#[cfg(target_arch = "wasm32")]
mod record;
#[cfg(target_arch = "wasm32")]
mod score;
#[cfg(target_arch = "wasm32")]
mod strategy;
#[cfg(target_arch = "wasm32")]
pub(crate) use history::*;
#[cfg(target_arch = "wasm32")]
pub(crate) use record::*;
#[cfg(target_arch = "wasm32")]
pub(crate) use score::*;
#[cfg(target_arch = "wasm32")]
pub(crate) use strategy::*;

