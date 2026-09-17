use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone)]
struct Node {
    id: String,
    score: f64,
    cost: u64,
    children: Vec<String>,
}

#[derive(Clone)]
struct Policy {
    id: String,
    roots: Vec<String>,
    max_nodes: usize,
}

struct World {
    id: String,
    nodes: BTreeMap<String, Node>,
}

fn string_field(value: &Value, field: &str) -> Result<String, String> {
    let value = value.get(field).and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("dream-replay requires non-empty {field}"))?;
    if !value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':')) {
        return Err(format!("dream-replay {field} must be an opaque ASCII identifier"));
    }
    Ok(value.to_string())
}

fn array_field<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>, String> {
    value.get(field).and_then(Value::as_array)
        .ok_or_else(|| format!("dream-replay requires array {field}"))
}

fn parse_policy(value: &Value) -> Result<Policy, String> {
    let id = string_field(value, "id")?;
    let roots = array_field(value, "roots")?.iter().map(|root| {
        root.as_str().filter(|value| !value.trim().is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| format!("dream-replay policy {id} has an empty root"))
    }).collect::<Result<Vec<_>, _>>()?;
    if roots.is_empty() {
        return Err(format!("dream-replay policy {id} requires at least one root"));
    }
    let max_nodes = value.get("max_nodes").and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| format!("dream-replay policy {id} requires positive safe max_nodes"))?;
    Ok(Policy { id, roots, max_nodes })
}

fn parse_world(value: &Value) -> Result<World, String> {
    let id = string_field(value, "id")?;
    let mut nodes = BTreeMap::new();
    for raw in array_field(value, "nodes")? {
        let node_id = string_field(raw, "id")?;
        if nodes.contains_key(&node_id) {
            return Err(format!("dream-replay world {id} repeats node {node_id}"));
        }
        let score = raw.get("score").and_then(Value::as_f64)
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("dream-replay node {node_id} requires finite score"))?;
        let cost = raw.get("cost").and_then(Value::as_u64)
            .ok_or_else(|| format!("dream-replay node {node_id} requires non-negative integer cost"))?;
        let children = array_field(raw, "children")?.iter().map(|child| {
            child.as_str().filter(|value| !value.trim().is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("dream-replay node {node_id} has an empty child"))
        }).collect::<Result<Vec<_>, _>>()?;
        nodes.insert(node_id.clone(), Node { id: node_id, score, cost, children });
    }
    if nodes.is_empty() {
        return Err(format!("dream-replay world {id} requires at least one node"));
    }
    for node in nodes.values() {
        for child in &node.children {
            if !nodes.contains_key(child) {
                return Err(format!("dream-replay world {id} node {} references unobserved child {child}", node.id));
            }
        }
    }
    Ok(World { id, nodes })
}

fn evaluate_policy(world: &World, policy: &Policy) -> Result<Value, String> {
    let mut queue = VecDeque::from(policy.roots.clone());
    let mut observed = BTreeSet::new();
    let mut score = 0.0;
    let mut cost = 0_u64;
    while let Some(id) = queue.pop_front() {
        if observed.len() >= policy.max_nodes || !observed.insert(id.clone()) {
            continue;
        }
        let node = world.nodes.get(&id)
            .ok_or_else(|| format!("dream-replay policy {} asks world {} for unobserved node {id}", policy.id, world.id))?;
        score += node.score;
        cost = cost.checked_add(node.cost).ok_or_else(|| "dream-replay cost overflow".to_string())?;
        for child in &node.children {
            if !observed.contains(child) {
                queue.push_back(child.clone());
            }
        }
    }
    if observed.is_empty() {
        return Err(format!("dream-replay policy {} observed no nodes in world {}", policy.id, world.id));
    }
    Ok(json!({
        "world_id": world.id,
        "observed_node_ids": observed.into_iter().collect::<Vec<_>>(),
        "score": score,
        "cost": cost,
    }))
}

fn evaluate_worlds(body: Value, worlds: Vec<World>) -> Result<Value, String> {
    let baseline_id = string_field(&body, "baseline_policy_id")?;
    let policies = array_field(&body, "policies")?.iter().map(parse_policy).collect::<Result<Vec<_>, _>>()?;
    let mut policy_ids = BTreeSet::new();
    for policy in &policies {
        if !policy_ids.insert(policy.id.clone()) {
            return Err(format!("dream-replay repeats policy {}", policy.id));
        }
    }
    if !policy_ids.contains(&baseline_id) {
        return Err(format!("dream-replay baseline_policy_id {baseline_id} is absent from policies"));
    }
    if worlds.is_empty() {
        return Err("dream-replay requires at least one sealed world".to_string());
    }
    let mut rankings = Vec::new();
    for policy in &policies {
        let replays = worlds.iter().map(|world| evaluate_policy(world, policy)).collect::<Result<Vec<_>, _>>()?;
        let score = replays.iter().filter_map(|replay| replay.get("score").and_then(Value::as_f64)).sum::<f64>();
        let cost = replays.iter().filter_map(|replay| replay.get("cost").and_then(Value::as_u64)).sum::<u64>();
        rankings.push(json!({ "policy_id": policy.id, "score": score, "cost": cost, "replays": replays }));
    }
    let baseline = rankings.iter().find(|ranking| ranking.get("policy_id").and_then(Value::as_str) == Some(baseline_id.as_str()))
        .cloned().ok_or_else(|| "dream-replay baseline ranking missing".to_string())?;
    let baseline_score = baseline.get("score").and_then(Value::as_f64).ok_or_else(|| "dream-replay baseline score missing".to_string())?;
    let selected = rankings.iter().filter(|ranking| ranking.get("score").and_then(Value::as_f64).unwrap_or(f64::NEG_INFINITY) > baseline_score)
        .max_by(|left, right| left.get("score").and_then(Value::as_f64).partial_cmp(&right.get("score").and_then(Value::as_f64)).unwrap_or(std::cmp::Ordering::Equal))
        .cloned().unwrap_or(baseline.clone());
    Ok(json!({
        "ok": true,
        "baseline_policy_id": baseline_id,
        "selected_policy_id": selected.get("policy_id").and_then(Value::as_str),
        "baseline_score": baseline_score,
        "selected_score": selected.get("score").and_then(Value::as_f64),
        "improved": selected.get("policy_id") != baseline.get("policy_id"),
        "rankings": rankings,
    }))
}

#[cfg(target_arch = "wasm32")]
const WORLD_PATH: &str = ".gm/dream-rsi/worlds.json";

#[cfg(target_arch = "wasm32")]
pub fn seal(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content).map_err(|error| format!("dream-world-seal requires JSON: {error}"))?;
    let world_id = string_field(&body, "world_id")?;
    let dispatch_ids = array_field(&body, "dispatch_ids")?;
    if dispatch_ids.is_empty() { return Err("dream-world-seal requires completed dispatch_ids".to_string()); }
    let cwd = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
    let nodes = dispatch_ids.iter().enumerate().map(|(index, value)| {
        let id = value.as_str().ok_or_else(|| "dream-world-seal dispatch_ids must contain strings".to_string())?;
        let entry = crate::dispatch_ledger::lookup(&cwd, id).ok_or_else(|| format!("dream-world-seal dispatch {id} is absent"))?;
        let exit_code = entry.get("exit_code").and_then(Value::as_i64).ok_or_else(|| format!("dream-world-seal dispatch {id} lacks exit code"))?;
        Ok(json!({ "id": id, "score": if exit_code == 0 { 1.0 } else { 0.0 }, "cost": 1, "children": if index + 1 < dispatch_ids.len() { vec![dispatch_ids[index + 1].as_str().unwrap_or("")] } else { vec![] } }))
    }).collect::<Result<Vec<_>, String>>()?;
    let world = parse_world(&json!({ "id": world_id, "nodes": nodes }))?;
    let raw = crate::pkfs::read_to_string(WORLD_PATH).unwrap_or_else(|| "[]".to_string());
    let mut worlds = serde_json::from_str::<Value>(&raw).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
    if worlds.iter().any(|entry| entry.get("id").and_then(Value::as_str) == Some(world.id.as_str())) { return Err(format!("dream-world-seal world {} already exists", world.id)); }
    worlds.push(json!({ "id": world.id, "dispatch_ids": dispatch_ids, "nodes": nodes }));
    if !crate::pkfs::write(WORLD_PATH, &Value::Array(worlds).to_string()) { return Err("dream-world-seal could not persist sealed world".to_string()); }
    Ok(json!({ "ok": true, "world_id": world.id, "node_count": world.nodes.len() }))
}

#[cfg(target_arch = "wasm32")]
pub fn evaluate(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content).map_err(|error| format!("dream-replay requires JSON: {error}"))?;
    if body.get("worlds").is_some() { return Err("dream-replay accepts sealed world_ids, never caller-supplied worlds".to_string()); }
    let ids = array_field(&body, "world_ids")?;
    let raw = crate::pkfs::read_to_string(WORLD_PATH).ok_or_else(|| "dream-replay has no sealed worlds".to_string())?;
    let stored = serde_json::from_str::<Value>(&raw).ok().and_then(|v| v.as_array().cloned()).ok_or_else(|| "dream-replay sealed world store is invalid".to_string())?;
    let worlds = ids.iter().map(|id| {
        let id = id.as_str().ok_or_else(|| "dream-replay world_ids must contain strings".to_string())?;
        let stored_world = stored.iter().find(|world| world.get("id").and_then(Value::as_str) == Some(id)).ok_or_else(|| format!("dream-replay sealed world {id} is absent"))?;
        parse_world(stored_world)
    }).collect::<Result<Vec<_>, _>>()?;
    evaluate_worlds(body, worlds)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn evaluate(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content).map_err(|error| format!("dream-replay requires JSON: {error}"))?;
    let worlds = array_field(&body, "worlds")?.iter().map(parse_world).collect::<Result<Vec<_>, _>>()?;
    evaluate_worlds(body, worlds)
}

#[cfg(target_arch = "wasm32")]
pub fn handle_seal(content: &str) -> (String, String, i32) {
    match seal(content) {
        Ok(result) => (result.to_string(), String::new(), 0),
        Err(error) => (json!({ "ok": false, "error": error }).to_string(), String::new(), 1),
    }
}

#[cfg(target_arch = "wasm32")]
pub fn handle(content: &str) -> (String, String, i32) {
    match evaluate(content) {
        Ok(result) => (result.to_string(), String::new(), 0),
        Err(error) => (json!({ "ok": false, "error": error }).to_string(), String::new(), 1),
    }
}
