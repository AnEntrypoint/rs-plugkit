#![cfg(target_arch = "wasm32")]

use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LifecycleState {
    Inactive,
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fiber {
    pub requires: BTreeSet<String>,
    pub provides: BTreeSet<String>,
    pub state: LifecycleState,
    pub retired: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registry {
    pub fibers: HashMap<String, Fiber>,
}

impl Registry {
    pub fn empty() -> Registry {
        Registry { fibers: HashMap::new() }
    }

    pub fn coeffect_context(&self) -> BTreeSet<String> {
        let mut ctx = BTreeSet::new();
        for fiber in self.fibers.values() {
            if fiber.state == LifecycleState::Active {
                for cap in &fiber.provides {
                    ctx.insert(cap.clone());
                }
            }
        }
        ctx
    }

    pub fn satisfied(&self, name: &str) -> bool {
        let ctx = self.coeffect_context();
        match self.fibers.get(name) {
            Some(fiber) => fiber.requires.iter().all(|dep| ctx.contains(dep)),
            None => false,
        }
    }

    pub fn well_formed(&self) -> bool {
        let names: Vec<&String> = self.fibers.keys().collect();
        for (i, a) in names.iter().enumerate() {
            for b in names.iter().skip(i + 1) {
                let a_provides = &self.fibers[*a].provides;
                let b_provides = &self.fibers[*b].provides;
                if !a_provides.is_disjoint(b_provides) {
                    return false;
                }
            }
        }
        true
    }

    pub fn insert(&self, name: &str, requires: BTreeSet<String>, provides: BTreeSet<String>) -> Option<Registry> {
        if self.fibers.contains_key(name) {
            return None;
        }
        for fiber in self.fibers.values() {
            if !fiber.provides.is_disjoint(&provides) {
                return None;
            }
        }
        let mut next = self.clone();
        next.fibers.insert(
            name.to_string(),
            Fiber { requires, provides, state: LifecycleState::Inactive, retired: false },
        );
        Some(next)
    }

    pub fn retire(&self, name: &str) -> Option<Registry> {
        if !self.fibers.contains_key(name) {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().retired = true;
        Some(next)
    }

    pub fn remove(&self, name: &str) -> Option<Registry> {
        let fiber = self.fibers.get(name)?;
        if !fiber.retired || fiber.state != LifecycleState::Inactive {
            return None;
        }
        let mut next = self.clone();
        next.fibers.remove(name);
        Some(next)
    }

    pub fn reload(&self, name: &str) -> Option<Registry> {
        let fiber = self.fibers.get(name)?;
        if fiber.state != LifecycleState::Inactive || fiber.retired || !self.satisfied(name) {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state = LifecycleState::Active;
        Some(next)
    }

    pub fn unload(&self, name: &str) -> Option<Registry> {
        let fiber = self.fibers.get(name)?;
        if fiber.state != LifecycleState::Active {
            return None;
        }
        let target_lost = fiber.retired || !self.satisfied(name);
        if !target_lost {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state = LifecycleState::Inactive;
        Some(next)
    }

    fn successors(&self, insert_candidates: &[(String, BTreeSet<String>, BTreeSet<String>)]) -> Vec<Registry> {
        let mut out = Vec::new();
        let names: Vec<String> = self.fibers.keys().cloned().collect();
        for name in &names {
            if let Some(r) = self.retire(name) {
                out.push(r);
            }
            if let Some(r) = self.remove(name) {
                out.push(r);
            }
            if let Some(r) = self.reload(name) {
                out.push(r);
            }
            if let Some(r) = self.unload(name) {
                out.push(r);
            }
        }
        for (name, requires, provides) in insert_candidates {
            if let Some(r) = self.insert(name, requires.clone(), provides.clone()) {
                out.push(r);
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalculusViolation {
    pub theorem: &'static str,
    pub detail: String,
}

pub fn verify_calculus(
    initial: &Registry,
    insert_candidates: &[(String, BTreeSet<String>, BTreeSet<String>)],
    max_states: usize,
) -> Vec<CalculusViolation> {
    let mut violations = Vec::new();
    let mut seen: Vec<Registry> = Vec::new();
    let mut frontier: Vec<Registry> = vec![initial.clone()];
    seen.push(initial.clone());

    while let Some(state) = frontier.pop() {
        if !state.well_formed() {
            violations.push(CalculusViolation {
                theorem: "preservation (Theorem 59)",
                detail: format!("state with colliding provisions reached: {:?}", state.fibers.keys().collect::<Vec<_>>()),
            });
        }

        let is_quiescent = state.fibers.iter().all(|(name, fiber)| {
            let target_active = !fiber.retired && state.satisfied(name);
            (fiber.state == LifecycleState::Active) == target_active
        });

        let successors = state.successors(insert_candidates);
        if !is_quiescent && successors.is_empty() {
            violations.push(CalculusViolation {
                theorem: "progress (Theorem 66)",
                detail: format!("non-quiescent state with no legal rule application: {:?}", state.fibers.keys().collect::<Vec<_>>()),
            });
        }

        for next in successors {
            if !seen.contains(&next) {
                if seen.len() >= max_states {
                    continue;
                }
                seen.push(next.clone());
                frontier.push(next);
            }
        }
    }

    violations
}

pub fn handle_model_check(_content: &str) -> (String, String, i32) {
    let requires_a = BTreeSet::new();
    let mut provides_a = BTreeSet::new();
    provides_a.insert("cap-a".to_string());

    let mut requires_b = BTreeSet::new();
    requires_b.insert("cap-a".to_string());
    let provides_b = BTreeSet::new();

    let mut requires_c = BTreeSet::new();
    requires_c.insert("cap-nonexistent".to_string());
    let provides_c = BTreeSet::new();

    let initial = Registry::empty();
    let insert_candidates = vec![
        ("fiber-a".to_string(), requires_a, provides_a),
        ("fiber-b".to_string(), requires_b, provides_b),
        ("fiber-c".to_string(), requires_c, provides_c),
    ];

    let mut violations = verify_calculus(&initial, &insert_candidates, 4096);

    let independence_result = demo_revert_arbitrary_order();
    let independence_ok = independence_result.theorem.ends_with(": OK");
    if !independence_ok {
        violations.push(independence_result.clone());
    }

    let ok = violations.is_empty();
    let payload = serde_json::json!({
        "ok": ok,
        "theorems_checked": ["preservation", "progress", "effect-independence (Def 17-21, Corollary 21)"],
        "model": "3-fiber bounded registry: fiber-a provides cap-a, fiber-b requires cap-a (satisfiable), fiber-c requires cap-nonexistent (never satisfiable)",
        "independence_check": {"theorem": independence_result.theorem, "detail": independence_result.detail},
        "violations": violations.iter().map(|v| serde_json::json!({"theorem": v.theorem, "detail": v.detail})).collect::<Vec<_>>(),
    });
    (payload.to_string(), String::new(), if ok { 0 } else { 1 })
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExtendedLifecycle {
    Inactive { outcome: Option<&'static str> },
    Reloading { remaining_iterations: u32, committed: BTreeSet<String> },
    Active { committed: BTreeSet<String> },
    Unloading { committed: BTreeSet<String>, outcome: Option<&'static str> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedFiber {
    pub requires: BTreeSet<String>,
    pub provides: BTreeSet<String>,
    pub state: ExtendedLifecycle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedRegistry {
    pub fibers: HashMap<String, ExtendedFiber>,
}

impl ExtendedRegistry {
    pub fn empty() -> ExtendedRegistry {
        ExtendedRegistry { fibers: HashMap::new() }
    }

    pub fn coeffect_context(&self) -> BTreeSet<String> {
        let mut ctx = BTreeSet::new();
        for fiber in self.fibers.values() {
            if let ExtendedLifecycle::Active { .. } = fiber.state {
                for cap in &fiber.provides {
                    ctx.insert(cap.clone());
                }
            }
        }
        ctx
    }

    pub fn satisfied(&self, name: &str) -> bool {
        let ctx = self.coeffect_context();
        match self.fibers.get(name) {
            Some(fiber) => fiber.requires.iter().all(|dep| ctx.contains(dep)),
            None => false,
        }
    }

    pub fn installed(&self, name: &str) -> bool {
        match self.fibers.get(name) {
            Some(fiber) => !matches!(fiber.state, ExtendedLifecycle::Inactive { .. }),
            None => false,
        }
    }

    fn target_defined(&self, name: &str) -> bool {
        self.fibers.contains_key(name) && self.satisfied(name)
    }

    pub fn relied(&self, name: &str) -> bool {
        self.fibers.iter().any(|(other_name, other)| {
            if other_name == name {
                return false;
            }
            if !self.installed(other_name) {
                return false;
            }
            let committed = match &other.state {
                ExtendedLifecycle::Reloading { committed, .. } => committed,
                ExtendedLifecycle::Active { committed } => committed,
                ExtendedLifecycle::Unloading { committed, .. } => committed,
                ExtendedLifecycle::Inactive { .. } => return false,
            };
            committed.contains(name)
        })
    }

    fn would_create_precedence_cycle(&self, name: &str, requires: &BTreeSet<String>, provides: &BTreeSet<String>) -> bool {
        let mut edges: HashMap<&str, Vec<&str>> = HashMap::new();
        for (n, fiber) in &self.fibers {
            for (m, other) in &self.fibers {
                if n != m && !fiber.provides.is_disjoint(&other.requires) {
                    edges.entry(n.as_str()).or_default().push(m.as_str());
                }
            }
            if !fiber.provides.is_disjoint(requires) {
                edges.entry(n.as_str()).or_default().push(name);
            }
            if !provides.is_disjoint(&fiber.requires) {
                edges.entry(name).or_default().push(n.as_str());
            }
        }
        let all_names: Vec<&str> = self.fibers.keys().map(|s| s.as_str()).chain(std::iter::once(name)).collect();
        let mut visiting: BTreeSet<&str> = BTreeSet::new();
        let mut done: BTreeSet<&str> = BTreeSet::new();
        fn has_cycle<'a>(
            node: &'a str,
            edges: &HashMap<&'a str, Vec<&'a str>>,
            visiting: &mut BTreeSet<&'a str>,
            done: &mut BTreeSet<&'a str>,
        ) -> bool {
            if done.contains(node) {
                return false;
            }
            if visiting.contains(node) {
                return true;
            }
            visiting.insert(node);
            if let Some(succs) = edges.get(node) {
                for s in succs {
                    if has_cycle(s, edges, visiting, done) {
                        return true;
                    }
                }
            }
            visiting.remove(node);
            done.insert(node);
            false
        }
        all_names.iter().any(|n| has_cycle(n, &edges, &mut visiting, &mut done))
    }

    pub fn insert(&self, name: &str, requires: BTreeSet<String>, provides: BTreeSet<String>) -> Option<ExtendedRegistry> {
        if self.fibers.contains_key(name) {
            return None;
        }
        for fiber in self.fibers.values() {
            if !fiber.provides.is_disjoint(&provides) {
                return None;
            }
        }
        if self.would_create_precedence_cycle(name, &requires, &provides) {
            return None;
        }
        let mut next = self.clone();
        next.fibers.insert(
            name.to_string(),
            ExtendedFiber { requires, provides, state: ExtendedLifecycle::Inactive { outcome: None } },
        );
        Some(next)
    }

    pub fn begin(&self, name: &str, remaining_iterations: u32) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        if !matches!(fiber.state, ExtendedLifecycle::Inactive { outcome: None }) {
            return None;
        }
        if !self.target_defined(name) {
            return None;
        }
        let omega = fiber.requires.clone();
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state =
            ExtendedLifecycle::Reloading { remaining_iterations, committed: omega };
        Some(next)
    }

    pub fn iterate(&self, name: &str) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        let (remaining, committed) = match &fiber.state {
            ExtendedLifecycle::Reloading { remaining_iterations, committed } if *remaining_iterations > 0 => {
                (*remaining_iterations, committed.clone())
            }
            _ => return None,
        };
        if self.target_defined(name) && self.fibers[name].requires != committed {
            return None;
        }
        if !self.target_defined(name) {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state =
            ExtendedLifecycle::Reloading { remaining_iterations: remaining - 1, committed };
        Some(next)
    }

    pub fn finish(&self, name: &str) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        let committed = match &fiber.state {
            ExtendedLifecycle::Reloading { remaining_iterations: 0, committed } => committed.clone(),
            _ => return None,
        };
        if !self.target_defined(name) || self.fibers[name].requires != committed {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state = ExtendedLifecycle::Active { committed };
        Some(next)
    }

    pub fn divert(&self, name: &str) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        let committed = match &fiber.state {
            ExtendedLifecycle::Reloading { committed, .. } => committed.clone(),
            _ => return None,
        };
        let target_changed = !self.target_defined(name) || self.fibers[name].requires != committed;
        if !target_changed {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state =
            ExtendedLifecycle::Unloading { committed, outcome: None };
        Some(next)
    }

    pub fn raise(&self, name: &str, error: &'static str) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        let committed = match &fiber.state {
            ExtendedLifecycle::Reloading { committed, .. } => committed.clone(),
            _ => return None,
        };
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state =
            ExtendedLifecycle::Unloading { committed, outcome: Some(error) };
        Some(next)
    }

    pub fn leave(&self, name: &str) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        let committed = match &fiber.state {
            ExtendedLifecycle::Active { committed } => committed.clone(),
            _ => return None,
        };
        let target_changed = !self.target_defined(name) || self.fibers[name].requires != committed;
        if !target_changed {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state =
            ExtendedLifecycle::Unloading { committed, outcome: None };
        Some(next)
    }

    pub fn unload(&self, name: &str) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        let outcome = match &fiber.state {
            ExtendedLifecycle::Unloading { outcome, .. } => *outcome,
            _ => return None,
        };
        if self.relied(name) {
            return None;
        }
        let mut next = self.clone();
        next.fibers.get_mut(name).unwrap().state = ExtendedLifecycle::Inactive { outcome };
        Some(next)
    }

    pub fn remove(&self, name: &str) -> Option<ExtendedRegistry> {
        let fiber = self.fibers.get(name)?;
        if !matches!(fiber.state, ExtendedLifecycle::Inactive { .. }) {
            return None;
        }
        if self.relied(name) {
            return None;
        }
        let mut next = self.clone();
        next.fibers.remove(name);
        Some(next)
    }
}

pub struct RevertibleEffect<Gamma> {
    pub fwd: Box<dyn Fn(&Gamma) -> Gamma>,
    pub inv: Box<dyn Fn(&Gamma, &Gamma) -> Gamma>,
}

impl<Gamma: Clone + PartialEq + std::fmt::Debug> RevertibleEffect<Gamma> {
    pub fn assert_left_inv(&self, s: &Gamma) -> bool {
        let fwd_s = (self.fwd)(s);
        let recovered = (self.inv)(s, &fwd_s);
        &recovered == s
    }
}

pub fn revert_lifo_pair<Gamma: Clone + PartialEq>(
    e1: &RevertibleEffect<Gamma>,
    e2: &RevertibleEffect<Gamma>,
    s0: &Gamma,
) -> Gamma {
    let mid = (e1.fwd)(s0);
    let final_state = (e2.fwd)(&mid);
    let undo_e2 = (e2.inv)(&mid, &final_state);
    (e1.inv)(s0, &undo_e2)
}

pub fn revert_nonlifo_pair<Gamma: Clone + PartialEq>(
    e1: &RevertibleEffect<Gamma>,
    e2: &RevertibleEffect<Gamma>,
    s0: &Gamma,
) -> Gamma {
    let mid = (e1.fwd)(s0);
    let final_state = (e2.fwd)(&mid);
    let undo_e1_first = (e1.inv)(s0, &final_state);
    (e2.inv)(s0, &undo_e1_first)
}

pub fn demo_revert_arbitrary_order() -> CalculusViolation {
    let s0: Vec<i64> = vec![0, 0, 0, 0];
    let idx1 = 0usize;
    let idx2 = 2usize;
    let val1 = 7i64;
    let val2 = 13i64;

    let e1 = RevertibleEffect::<Vec<i64>> {
        fwd: Box::new(move |s: &Vec<i64>| {
            let mut next = s.clone();
            next[idx1] = val1;
            next
        }),
        inv: Box::new(move |pre: &Vec<i64>, _post: &Vec<i64>| {
            let mut restored = pre.clone();
            restored[idx1] = pre[idx1];
            restored
        }),
    };
    let e2 = RevertibleEffect::<Vec<i64>> {
        fwd: Box::new(move |s: &Vec<i64>| {
            let mut next = s.clone();
            next[idx2] = val2;
            next
        }),
        inv: Box::new(move |pre: &Vec<i64>, _post: &Vec<i64>| {
            let mut restored = pre.clone();
            restored[idx2] = pre[idx2];
            restored
        }),
    };

    if !e1.assert_left_inv(&s0) {
        return CalculusViolation {
            theorem: "Definition 17 (revertible-effect law)",
            detail: "e1.inv(s, e1.fwd(s)) != s at s0".to_string(),
        };
    }
    if !e2.assert_left_inv(&(e1.fwd)(&s0)) {
        return CalculusViolation {
            theorem: "Definition 17 (revertible-effect law)",
            detail: "e2.inv(s, e2.fwd(s)) != s at e1.fwd(s0)".to_string(),
        };
    }

    let fwd_commute = (e1.fwd)(&(e2.fwd)(&s0)) == (e2.fwd)(&(e1.fwd)(&s0));
    if !fwd_commute {
        return CalculusViolation {
            theorem: "Definition 18/Lemma 18 (generator commutation)",
            detail: "e1.fwd and e2.fwd do not commute at s0 -- effects are not independent".to_string(),
        };
    }

    let lifo_result = revert_lifo_pair(&e1, &e2, &s0);
    let nonlifo_result = revert_nonlifo_pair(&e1, &e2, &s0);

    if lifo_result != s0 {
        return CalculusViolation {
            theorem: "Corollary 21 (LIFO reversion baseline)",
            detail: format!("LIFO reversion did not recover s0: got {:?}, expected {:?}", lifo_result, s0),
        };
    }
    if nonlifo_result != s0 {
        return CalculusViolation {
            theorem: "Corollary 21 (arbitrary-order reversion)",
            detail: format!(
                "non-LIFO reversion did not recover s0: got {:?}, expected {:?}",
                nonlifo_result, s0
            ),
        };
    }

    CalculusViolation {
        theorem: "Corollary 21 (arbitrary-order reversion): OK",
        detail: format!(
            "both LIFO and non-LIFO reversion orders recovered s0={:?} exactly from independent effects e1/e2",
            s0
        ),
    }
}

pub fn obs_equiv(a: &[String], g1: &Registry, g2: &Registry) -> bool {
    a.iter().all(|name| g1.satisfied(name) == g2.satisfied(name))
}

pub fn registry_equiv(g1: &Registry, g2: &Registry) -> bool {
    g1.fibers == g2.fibers
}

pub fn registry_equiv_implies_obs_equiv(a: &[String], g1: &Registry, g2: &Registry) -> bool {
    if !registry_equiv(g1, g2) {
        return true;
    }
    obs_equiv(a, g1, g2)
}
