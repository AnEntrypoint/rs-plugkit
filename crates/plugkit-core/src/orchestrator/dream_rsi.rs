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

fn evaluate_worlds(baseline_id: String, policies: Vec<Policy>, worlds: Vec<World>) -> Result<Value, String> {
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
const DISCOVERY_PATH: &str = ".gm/dream-rsi/discovery.json";
#[cfg(target_arch = "wasm32")]
const POLICY_PATH: &str = ".gm/dream-rsi/policies.json";

#[cfg(target_arch = "wasm32")]
fn session_id() -> Result<String, String> {
    crate::orchestrator::state::dispatch_session_id().filter(|id| !id.trim().is_empty())
        .ok_or_else(|| "Dream-RSI requires a gm session ID".to_string())
}

#[cfg(target_arch = "wasm32")]
fn signed_record(kind: &str, record: Value) -> Result<Value, String> {
    let key = crate::pipeline::hmac_key()?;
    let payload = json!({ "kind": kind, "record": record }).to_string();
    Ok(json!({ "record": record, "mac": crate::pipeline::keyed_hash(&key, &payload) }))
}

#[cfg(target_arch = "wasm32")]
fn verify_record(kind: &str, signed: &Value) -> Result<Value, String> {
    let record = signed.get("record").cloned().ok_or_else(|| format!("Dream-RSI {kind} record is missing"))?;
    let actual = signed.get("mac").and_then(Value::as_str).ok_or_else(|| format!("Dream-RSI {kind} record MAC is missing"))?;
    let key = crate::pipeline::hmac_key()?;
    let expected = crate::pipeline::keyed_hash(&key, &json!({ "kind": kind, "record": record }).to_string());
    if actual.len() != expected.len() || !actual.bytes().zip(expected.bytes()).fold(0u8, |diff, (left, right)| diff | (left ^ right)).eq(&0) { return Err(format!("Dream-RSI {kind} record integrity check failed")); }
    Ok(record)
}

#[cfg(target_arch = "wasm32")]
pub fn register_policy(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content).map_err(|error| format!("dream-policy-register requires JSON: {error}"))?;
    let policy = parse_policy(body.get("policy").ok_or_else(|| "dream-policy-register requires policy".to_string())?)?;
    let owner_session_id = session_id()?;
    let deployed = body.get("deployed").and_then(Value::as_bool).unwrap_or(false);
    let raw = crate::pkfs::read_to_string(POLICY_PATH).unwrap_or_else(|| "[]".to_string());
    let mut policies = serde_json::from_str::<Value>(&raw).map_err(|_| "dream-policy-register policy store is invalid".to_string())?.as_array().cloned().ok_or_else(|| "dream-policy-register policy store is invalid".to_string())?.into_iter().map(|entry| verify_record("policy", &entry)).collect::<Result<Vec<_>, _>>()?;
    if policies.iter().any(|entry| entry.get("id").and_then(Value::as_str) == Some(policy.id.as_str()) && entry.get("owner_session_id").and_then(Value::as_str) == Some(owner_session_id.as_str())) { return Err(format!("dream-policy-register policy {} already exists", policy.id)); }
    if deployed { for entry in &mut policies { if entry.get("owner_session_id").and_then(Value::as_str) == Some(owner_session_id.as_str()) { entry["deployed"] = Value::Bool(false); } } }
    let new_policy = json!({ "id": policy.id, "owner_session_id": owner_session_id, "roots": policy.roots, "max_nodes": policy.max_nodes, "deployed": deployed });
    policies.push(new_policy);
    let signed = policies.into_iter().map(|record| signed_record("policy", record)).collect::<Result<Vec<_>, _>>()?;
    if !crate::pkfs::write(POLICY_PATH, &Value::Array(signed).to_string()) { return Err("dream-policy-register could not persist policy".to_string()); }
    Ok(json!({ "ok": true, "policy_id": policy.id, "deployed": deployed }))
}

#[cfg(target_arch = "wasm32")]
pub fn evaluator_receipt(_content: &str) -> Result<Value, String> {
    Err("dream-evaluator-receipt requires a deployment-owned evaluator provider; caller-supplied target, score, and cost are not admissible evidence".to_string())
}

#[cfg(target_arch = "wasm32")]
pub fn record_discovery(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content).map_err(|error| format!("dream-discovery-record requires JSON: {error}"))?;
    let id = string_field(&body, "id")?;
    let owner_session_id = session_id()?;
    let evaluator = verify_record("evaluator", body.get("evaluator_receipt").ok_or_else(|| "dream-discovery-record requires evaluator_receipt".to_string())?)?;
    let target = string_field(&evaluator, "target")?;
    let policy_id = string_field(&evaluator, "policy_id")?;
    let dispatch_id = string_field(&evaluator, "dispatch_id")?;
    let evaluator_score = evaluator.get("evaluator_score").and_then(Value::as_f64).filter(|value| value.is_finite()).ok_or_else(|| "dream-discovery-record evaluator receipt requires finite evaluator_score".to_string())?;
    let cost = evaluator.get("cost").and_then(Value::as_u64).ok_or_else(|| "dream-discovery-record evaluator receipt requires non-negative integer cost".to_string())?;
    let parent_id = evaluator.get("parent_id").map(|_| string_field(&evaluator, "parent_id")).transpose()?;
    let raw_policies = crate::pkfs::read_to_string(POLICY_PATH).ok_or_else(|| "dream-discovery-record has no registered policies".to_string())?;
    let policies = serde_json::from_str::<Value>(&raw_policies).map_err(|_| "dream-discovery-record policy store is invalid".to_string())?.as_array().cloned().ok_or_else(|| "dream-discovery-record policy store is invalid".to_string())?.into_iter().map(|entry| verify_record("policy", &entry)).collect::<Result<Vec<_>, _>>()?;
    if !policies.iter().any(|policy| policy.get("id").and_then(Value::as_str) == Some(policy_id.as_str()) && policy.get("owner_session_id").and_then(Value::as_str) == Some(owner_session_id.as_str())) { return Err("dream-discovery-record policy_id is not registered in this session".to_string()); }
    let cwd = crate::wasm_dispatch::host_cwd_string().unwrap_or_default();
    let dispatch = crate::dispatch_ledger::lookup(&cwd, &dispatch_id).ok_or_else(|| "dream-discovery-record dispatch_id is not a completed gm dispatch".to_string())?;
    if dispatch.get("exit_code").and_then(Value::as_i64) != Some(0) { return Err("dream-discovery-record requires a successful completed dispatch".to_string()); }
    let raw = crate::pkfs::read_to_string(DISCOVERY_PATH).unwrap_or_else(|| "[]".to_string());
    let mut records = serde_json::from_str::<Value>(&raw).map_err(|_| "dream-discovery-record store is invalid".to_string())?.as_array().cloned().ok_or_else(|| "dream-discovery-record store is invalid".to_string())?.into_iter().map(|entry| verify_record("discovery", &entry)).collect::<Result<Vec<_>, _>>()?;
    if records.iter().any(|record| record.get("id").and_then(Value::as_str) == Some(id.as_str())) { return Err(format!("dream-discovery-record {id} already exists")); }
    if let Some(parent_id) = &parent_id {
        if !records.iter().any(|record| record.get("id").and_then(Value::as_str) == Some(parent_id.as_str()) && record.get("owner_session_id").and_then(Value::as_str) == Some(owner_session_id.as_str())) { return Err(format!("dream-discovery-record parent {parent_id} is absent")); }
    }
    let record = json!({ "id": id, "owner_session_id": owner_session_id, "target": target, "policy_id": policy_id, "dispatch_id": dispatch_id, "evaluator_score": evaluator_score, "cost": cost, "parent_id": parent_id });
    records.push(record.clone());
    let signed = records.into_iter().map(|record| signed_record("discovery", record)).collect::<Result<Vec<_>, _>>()?;
    if !crate::pkfs::write(DISCOVERY_PATH, &Value::Array(signed).to_string()) { return Err("dream-discovery-record could not persist discovery record".to_string()); }
    Ok(json!({ "ok": true, "discovery_id": id }))
}

#[cfg(target_arch = "wasm32")]
pub fn seal(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content).map_err(|error| format!("dream-world-seal requires JSON: {error}"))?;
    let world_id = string_field(&body, "world_id")?;
    let owner_session_id = session_id()?;
    let discovery_ids = array_field(&body, "discovery_ids")?;
    if discovery_ids.is_empty() { return Err("dream-world-seal requires recorded discovery_ids".to_string()); }
    let raw_records = crate::pkfs::read_to_string(DISCOVERY_PATH).ok_or_else(|| "dream-world-seal has no discovery records".to_string())?;
    let records = serde_json::from_str::<Value>(&raw_records).map_err(|_| "dream-world-seal discovery record store is invalid".to_string())?.as_array().cloned().ok_or_else(|| "dream-world-seal discovery record store is invalid".to_string())?.into_iter().map(|entry| verify_record("discovery", &entry)).collect::<Result<Vec<_>, _>>()?;
    let nodes = discovery_ids.iter().enumerate().map(|(index, value)| {
        let id = value.as_str().ok_or_else(|| "dream-world-seal discovery_ids must contain strings".to_string())?;
        let record = records.iter().find(|record| record.get("id").and_then(Value::as_str) == Some(id)).ok_or_else(|| format!("dream-world-seal discovery {id} is absent"))?;
        if record.get("owner_session_id").and_then(Value::as_str) != Some(owner_session_id.as_str()) { return Err(format!("dream-world-seal discovery {id} belongs to another session")); }
        let score = record.get("evaluator_score").and_then(Value::as_f64).filter(|value| value.is_finite()).ok_or_else(|| format!("dream-world-seal discovery {id} lacks evaluator score"))?;
        let cost = record.get("cost").and_then(Value::as_u64).ok_or_else(|| format!("dream-world-seal discovery {id} lacks cost"))?;
        Ok(json!({ "id": id, "score": score, "cost": cost, "children": if index + 1 < discovery_ids.len() { vec![discovery_ids[index + 1].as_str().unwrap_or("")] } else { vec![] } }))
    }).collect::<Result<Vec<_>, String>>()?;
    let world = parse_world(&json!({ "id": world_id, "nodes": nodes }))?;
    let raw = crate::pkfs::read_to_string(WORLD_PATH).unwrap_or_else(|| "[]".to_string());
    let mut worlds = serde_json::from_str::<Value>(&raw).map_err(|_| "dream-world-seal world store is invalid".to_string())?.as_array().cloned().ok_or_else(|| "dream-world-seal world store is invalid".to_string())?.into_iter().map(|entry| verify_record("world", &entry)).collect::<Result<Vec<_>, _>>()?;
    if worlds.iter().any(|entry| entry.get("id").and_then(Value::as_str) == Some(world.id.as_str())) { return Err(format!("dream-world-seal world {} already exists", world.id)); }
    worlds.push(json!({ "id": world.id, "owner_session_id": owner_session_id, "discovery_ids": discovery_ids, "nodes": nodes }));
    let signed = worlds.into_iter().map(|record| signed_record("world", record)).collect::<Result<Vec<_>, _>>()?;
    if !crate::pkfs::write(WORLD_PATH, &Value::Array(signed).to_string()) { return Err("dream-world-seal could not persist sealed world".to_string()); }
    Ok(json!({ "ok": true, "world_id": world.id, "node_count": world.nodes.len() }))
}

#[cfg(target_arch = "wasm32")]
pub fn evaluate(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content).map_err(|error| format!("dream-replay requires JSON: {error}"))?;
    let owner_session_id = session_id()?;
    if body.get("worlds").is_some() { return Err("dream-replay accepts sealed world_ids, never caller-supplied worlds".to_string()); }
    let ids = array_field(&body, "world_ids")?;
    let baseline_id = string_field(&body, "baseline_policy_id")?;
    let policy_ids = array_field(&body, "policy_ids")?;
    let raw_policies = crate::pkfs::read_to_string(POLICY_PATH).ok_or_else(|| "dream-replay has no registered policies".to_string())?;
    let stored_policies = serde_json::from_str::<Value>(&raw_policies).map_err(|_| "dream-replay policy store is invalid".to_string())?.as_array().cloned().ok_or_else(|| "dream-replay policy store is invalid".to_string())?.into_iter().map(|entry| verify_record("policy", &entry)).collect::<Result<Vec<_>, _>>()?;
    let policies = policy_ids.iter().map(|id| {
        let id = id.as_str().ok_or_else(|| "dream-replay policy_ids must contain strings".to_string())?;
        let stored_policy = stored_policies.iter().find(|policy| policy.get("id").and_then(Value::as_str) == Some(id) && policy.get("owner_session_id").and_then(Value::as_str) == Some(owner_session_id.as_str())).ok_or_else(|| format!("dream-replay registered policy {id} is absent"))?;
        parse_policy(stored_policy)
    }).collect::<Result<Vec<_>, _>>()?;
    if !stored_policies.iter().any(|policy| policy.get("id").and_then(Value::as_str) == Some(baseline_id.as_str()) && policy.get("owner_session_id").and_then(Value::as_str) == Some(owner_session_id.as_str()) && policy.get("deployed").and_then(Value::as_bool) == Some(true)) { return Err("dream-replay baseline_policy_id is not this session's deployed policy".to_string()); }
    let raw = crate::pkfs::read_to_string(WORLD_PATH).ok_or_else(|| "dream-replay has no sealed worlds".to_string())?;
    let stored = serde_json::from_str::<Value>(&raw).map_err(|_| "dream-replay sealed world store is invalid".to_string())?.as_array().cloned().ok_or_else(|| "dream-replay sealed world store is invalid".to_string())?.into_iter().map(|entry| verify_record("world", &entry)).collect::<Result<Vec<_>, _>>()?;
    let worlds = ids.iter().map(|id| {
        let id = id.as_str().ok_or_else(|| "dream-replay world_ids must contain strings".to_string())?;
        let stored_world = stored.iter().find(|world| world.get("id").and_then(Value::as_str) == Some(id)).ok_or_else(|| format!("dream-replay sealed world {id} is absent"))?;
        if stored_world.get("owner_session_id").and_then(Value::as_str) != Some(owner_session_id.as_str()) { return Err(format!("dream-replay sealed world {id} belongs to another session")); }
        parse_world(stored_world)
    }).collect::<Result<Vec<_>, _>>()?;
    evaluate_worlds(baseline_id, policies, worlds)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn evaluate(content: &str) -> Result<Value, String> {
    let body: Value = serde_json::from_str(content).map_err(|error| format!("dream-replay requires JSON: {error}"))?;
    let baseline_id = string_field(&body, "baseline_policy_id")?;
    let policies = array_field(&body, "policies")?.iter().map(parse_policy).collect::<Result<Vec<_>, _>>()?;
    let worlds = array_field(&body, "worlds")?.iter().map(parse_world).collect::<Result<Vec<_>, _>>()?;
    evaluate_worlds(baseline_id, policies, worlds)
}

#[cfg(target_arch = "wasm32")]
pub fn handle_policy_register(content: &str) -> (String, String, i32) {
    match register_policy(content) {
        Ok(result) => (result.to_string(), String::new(), 0),
        Err(error) => (json!({ "ok": false, "error": error }).to_string(), String::new(), 1),
    }
}

#[cfg(target_arch = "wasm32")]
pub fn handle_evaluator_receipt(content: &str) -> (String, String, i32) {
    match evaluator_receipt(content) {
        Ok(result) => (result.to_string(), String::new(), 0),
        Err(error) => (json!({ "ok": false, "error": error }).to_string(), String::new(), 1),
    }
}

#[cfg(target_arch = "wasm32")]
pub fn handle_discovery_record(content: &str) -> (String, String, i32) {
    match record_discovery(content) {
        Ok(result) => (result.to_string(), String::new(), 0),
        Err(error) => (json!({ "ok": false, "error": error }).to_string(), String::new(), 1),
    }
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
