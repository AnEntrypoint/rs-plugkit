use super::*;

#[derive(Default)]
pub(crate) struct LedgerCheck {
    pub(crate) entries: std::collections::HashMap<String, Value>,
    pub(crate) unreadable: Option<&'static str>,
    pub(crate) evicted_possible: bool,
    pub(crate) newest_ts: u64,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Confirmation {
    Ledger,
    Retained,
}

impl LedgerCheck {
    pub(crate) fn load() -> Self {
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

    pub(crate) fn verdict(&mut self, session_id: &str, record: &ObservationRecord) -> Result<Confirmation, &'static str> {
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

#[derive(Default)]
pub(crate) struct HistoryScan {
    pub(crate) worlds: Vec<World>,
    pub(crate) sessions: usize,
    pub(crate) observations: usize,
    pub(crate) ledger_confirmed: usize,
    pub(crate) spool_out_confirmed: usize,
    pub(crate) dropped: BTreeMap<&'static str, usize>,
    pub(crate) rejected: usize,
    pub(crate) latest: Option<(i64, String)>,
    pub(crate) windows: BTreeMap<String, BTreeSet<String>>,
}

pub(crate) const HISTORY_ROOT_NODE: &str = "start";

pub(crate) fn session_world(session_id: &str, records: &[ObservationRecord]) -> Option<World> {
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

pub(crate) const SESSION_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

pub(crate) fn newest_entry_mtime_ms(dir: &str) -> Option<i64> {
    dir_entry_names(dir)
        .iter()
        .filter_map(|name| crate::pkfs::stat(&format!("{dir}/{name}")))
        .filter_map(|s| s.get("mtime_ms").or_else(|| s.get("mtimeMs"))?.as_f64())
        .map(|ms| ms as i64)
        .max()
}

pub(crate) fn open_row_sessions() -> Option<std::collections::HashSet<String>> {
    let (body, _, code) = super::super::prd::handle_list_full();
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
                    .is_some_and(super::super::prd::status_is_open)
            })
            .filter_map(|row| row.get("session_id")?.as_str().map(str::to_owned))
            .collect(),
    )
}

pub(crate) fn prune_idle_sessions(now_ms: i64, keep: &str) {
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

pub(crate) fn scan_history() -> HistoryScan {
    let mut ledger = LedgerCheck::load();
    let mut scan = HistoryScan::default();
    prune_idle_sessions(
        super::super::state::now_ms() as i64,
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

pub(crate) const DEFAULT_INCUMBENT_POLICY_ID: &str = "pi0-default";

pub(crate) fn incumbent_policy(owner: &str) -> Policy {
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

pub(crate) fn dream_candidates(incumbent: &Policy) -> Vec<Policy> {
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

pub(crate) fn policy_json(policy: &Policy) -> Value {
    json!({
        "id": policy.id, "roots": policy.roots, "max_nodes": policy.max_nodes,
        "group_size": policy.group_size, "stop": policy.stop.map(Stop::name), "attempt": policy.attempt,
    })
}

pub const POSTCONDITION_FAILED: &str = "dream-replay-cycle postcondition failed";

pub const DEFAULT_CYCLE_BETA1: f64 = 0.01;

pub const DEFAULT_CYCLE_BETA2: f64 = 0.05;

pub struct HistoryRound {
    pub latest_dispatch_id: Option<String>,
    pub summary: Value,
    pub lost_cursor: Option<String>,
    pub no_replayable_worlds: bool,
    pub scores: Option<ReplayScores>,
}

pub(crate) fn history_round(owner: &str, cursor: Option<&str>, beta1: f64, beta2: f64) -> Result<HistoryRound, String> {
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
            scores: None,
        });
    }
    let candidate_count = candidates.len();
    let promotion_margin = crate::ragconfig::RagConfig::resolved().scoring.promotion_margin;
    let replay = evaluate_worlds(incumbent.id.clone(), candidates.clone(), scan.worlds, beta1, beta2, promotion_margin)?;
    let baseline_score = finite_score(replay.baseline_score).ok_or("dream-replay-cycle baseline score missing")?;
    let selected_id = replay.selected.policy_id.clone();
    let selected_score = finite_score(replay.selected.score).ok_or("dream-replay-cycle selected score missing")?;
    let ranked_score = |id: &str| {
        replay
            .rankings
            .iter()
            .find(|ranking| ranking.policy_id == id)
            .and_then(|ranking| finite_score(ranking.score))
    };
    let never_worse = matches!(
        (ranked_score(&selected_id), ranked_score(&incumbent.id)),
        (Some(selected), Some(incumbent_score)) if selected >= incumbent_score && selected_score >= baseline_score
    );
    if !never_worse {
        defects.push("postcondition_selected_below_incumbent");
    }
    let mut rankings: Vec<Value> = replay
        .rankings
        .iter()
        .map(|ranking| json!({ "policy_id": ranking.policy_id, "score": ranking.score, "cost": ranking.cost }))
        .collect();
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
        scores: Some(ReplayScores {
            baseline: Some(baseline_score),
            selected_policy_id: selected_id.clone(),
            selected: Some(selected_score),
            ranked: replay.rankings.iter().map(|ranking| (ranking.policy_id.clone(), ranking.score)).collect(),
        }),
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

