#![cfg(target_arch = "wasm32")]

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Scope note: Algorithm 3 (paper Section 5.1.2, `notify(ctx, keys)`) is
/// deliberately NOT implemented as push-based propagation in this crate.
/// Algorithm 3 iterates `all_fibers`, tests `key in fiber.inject` against
/// a same-realm match, and calls `refresh` on each match, so a caller
/// that installed or withdrew a binding can await exactly the fibers the
/// change affected. This buys something concrete only in a runtime where
/// components can activate/deactivate ASYNCHRONOUSLY relative to a
/// `set`/`get` call -- Definition 26's own text frames `notify_d` against
/// "diverse control flows" (Section 5.1.3) precisely because the paper's
/// target runtime is event-driven.
///
/// gm's orchestrator has no such runtime. `discipline_note.rs`'s
/// `active_policies()` is the ONLY call site that reads/advances
/// discipline fiber state (grep-verified: zero other callers), and it
/// runs exactly once per `instruction` verb dispatch -- the sole state
/// -mutating entry point an agent drives. There is no concurrent process,
/// timer, or background task that can flip a binding's satisfaction
/// between two `instruction` calls; the state machine only moves when the
/// agent itself issues the next spool dispatch. Under that constraint,
/// full re-derivation on every dispatch (`active_policies`'s two-phase
/// snapshot-then-advance pass) is a correctness-equivalent realization of
/// Definition 26's reactive invariant ("every coeffect change is
/// observed"): every state transition IS an `instruction` dispatch, so
/// every transition is observed by construction, with no notify queue
/// needed to avoid missing one. A push-based `notify` here would recompute
/// the identical activate/deactivate classification `active_policies`
/// already derives fresh each call, at strictly higher implementation
/// cost (a fiber-to-key subscription index, an event queue, ordering
/// rules for concurrent notifications) for zero additional correctness:
/// there is no window in which pull-based re-derivation could observe a
/// stale satisfaction status that push-based notify would have caught,
/// because nothing changes outside an `instruction` dispatch's own
/// two-phase pass.
///
/// This divergence does NOT weaken the one thing Algorithm 3 buys beyond
/// bare re-derivation -- ordering a withdrawal against its dependents
/// (paper Section 4.3.1, Theorem 63, "the converse fails" paragraph under
/// Definition 26). `discipline_note.rs::removal_dependents` (the
/// withdrawal-ordering guard behind the `discipline-check-removal` verb)
/// enforces that ordering PRE-EMPTIVELY, before `enabled.txt` is ever
/// mutated, by naming every still-Active same-realm dependent that would
/// break -- strictly stronger than Algorithm 3's `notify`, which only
/// detects a broken dependent POST-HOC, after the withdrawal already
/// happened. Formal correspondence commentary:
/// `rs-plugkit/formal/CordisCalculus/Isolation.lean`.
///
/// Contrast with this crate's genuine scope-outs (cross-process RPC
/// coeffect propagation, bridge-fiber sandboxing): those diverge because
/// gm's actual deployment lacks a mechanism the paper's model assumes
/// (a message bus spanning process boundaries, an OS-level sandbox
/// primitive). This divergence is the opposite shape -- gm's dispatch
/// model makes Algorithm 3's own mechanism (queueing which fibers to
/// notify, for a caller to later await) provably redundant, not
/// unreachable.

/// Coeffect isolation (paper Section 3.2.3, Definition 28-29): the
/// realm table `rho : K -> R` (`realm_table` here) plus the dependency
/// table `sigma : R -> V_r` (`by_realm` here) it resolves into,
/// modeled as the pair `Sigma^iso := (K -> R) x ((r:R) -> V_r)`.
/// `discipline_note.rs`'s own `declared_realm` is a coarser
/// one-realm-per-discipline reduction of this same idea; this type is
/// the paper's full per-key realm table, used wherever a caller needs
/// individual capability KEYS (not whole disciplines) to resolve
/// against different realms.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RealmTable {
    #[serde(default)]
    realm_table: BTreeMap<String, String>,
    #[serde(default)]
    by_realm: BTreeMap<String, BTreeMap<String, String>>,
}

impl RealmTable {
    pub fn new() -> RealmTable {
        RealmTable::default()
    }

    /// `rho(k)`: a key outside `dom(rho)` resolves to its own realm
    /// (the empty-string default realm), matching Definition 28's own
    /// text ("a key outside dom(rho) resolves to its own realm, so we
    /// write rho(k) = k there").
    pub fn realm_of(&self, key: &str) -> String {
        self.realm_table.get(key).cloned().unwrap_or_default()
    }

    /// Definition 29 `get`: `get(k)(rho,sigma) = sigma(rho(k))`.
    pub fn get(&self, key: &str) -> Option<&String> {
        let realm = self.realm_of(key);
        self.by_realm.get(&realm).and_then(|table| table.get(key))
    }

    /// Definition 29 `set`: writes `sigma[rho(k) -> v]`, requiring
    /// `rho(k) \notin dom(sigma)` as Definition 23's precondition,
    /// transported along `rho` per the paper's own text. Returns
    /// `false` (a no-op, matching the paper's error-signalling
    /// convention for a violated precondition) when the binding
    /// already exists.
    pub fn set(&mut self, key: &str, value: String) -> bool {
        let realm = self.realm_of(key);
        let table = self.by_realm.entry(realm).or_default();
        if table.contains_key(key) {
            return false;
        }
        table.insert(key.to_string(), value);
        true
    }

    /// Definition 29 `isolate(k, r)`: `rho[k -> r]`, inheriting the
    /// dependency table unchanged -- a *derived* realization
    /// (Definition 27): no precondition, "a key already isolated is
    /// reassigned rather than refused."
    pub fn isolate(&mut self, key: &str, realm: &str) {
        self.realm_table.insert(key.to_string(), realm.to_string());
    }

    pub fn realm_table(&self) -> &BTreeMap<String, String> {
        &self.realm_table
    }
}

/// Coeffect interception (paper Section 3.2.3, Definition 30-31):
/// `Sigma^inter := ((k:K) -> M_k) x ((k:K) -> (M_k -> V_k))`. `iota`
/// (`context_carried`) is the context-carried metadata installed on
/// the context itself, `epsilon_k` (empty) by default; the provider
/// side (`sigma`, the actual `V_k` values) is out of scope for this
/// type -- it lives wherever the caller's own `get`/`set` already
/// stores values (e.g. `RealmTable` above, or a discipline's declared
/// capability). This type owns only `iota`'s merge machinery, kept
/// separate from value storage the same way Definition 30 keeps
/// `Sigma^inter`'s two components independent.
///
/// `merge_kind` records each key's `(M_k, +_k, epsilon_k)` monoid
/// shape declaratively (Definition 31's text: "this merge follows each
/// key's own semantics, e.g. scalar fields are overwritten, set-valued
/// fields are unioned"), since a wasm-hosted coeffect table cannot
/// carry an arbitrary Rust closure as `+_k` across the spool-JSON
/// boundary -- `MergeKind` names the finite family of `+_k` shapes this
/// crate needs, each one a genuine associative operation with the
/// stated identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeKind {
    /// Right-biased overwrite: `a +_k b = if b is empty then a else b`.
    /// Identity is the empty string. Associative (a straightforward
    /// case analysis on which of three operands is empty).
    ScalarOverwrite,
    /// Set union of comma-separated tokens, deduplicated and sorted for
    /// a canonical representative -- associative and commutative, with
    /// the empty string as identity.
    SetUnion,
}

impl MergeKind {
    fn identity(self) -> String {
        String::new()
    }

    /// `+_k`, right-biased per Definition 31's text ("this merge ...
    /// is right-biased, so `iota(k)` takes priority and can override
    /// the component's declaration").
    fn combine(self, left: &str, right: &str) -> String {
        match self {
            MergeKind::ScalarOverwrite => {
                if right.is_empty() {
                    left.to_string()
                } else {
                    right.to_string()
                }
            }
            MergeKind::SetUnion => {
                let mut items: Vec<&str> = left
                    .split(',')
                    .chain(right.split(','))
                    .filter(|s| !s.is_empty())
                    .collect();
                items.sort_unstable();
                items.dedup();
                items.join(",")
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InterceptionContext {
    /// `iota`: context-carried metadata per key, `epsilon_k` (empty
    /// string) by default when a key is absent.
    #[serde(default)]
    context_carried: BTreeMap<String, String>,
    /// Each key's declared `(M_k, +_k, epsilon_k)` monoid shape
    /// (Definition 30's requirement that every key equip its metadata
    /// with a monoid); a key with no declared kind defaults to
    /// `ScalarOverwrite`, the paper's own leading example.
    #[serde(default)]
    merge_kind: BTreeMap<String, MergeKind>,
}

impl InterceptionContext {
    pub fn new() -> InterceptionContext {
        InterceptionContext::default()
    }

    pub fn declare_merge_kind(&mut self, key: &str, kind: MergeKind) {
        self.merge_kind.insert(key.to_string(), kind);
    }

    fn kind_of(&self, key: &str) -> MergeKind {
        self.merge_kind.get(key).copied().unwrap_or(MergeKind::ScalarOverwrite)
    }

    /// `iota(k)`, `epsilon_k` when the key carries no context metadata.
    pub fn context_metadata(&self, key: &str) -> String {
        self.context_carried
            .get(key)
            .cloned()
            .unwrap_or_else(|| self.kind_of(key).identity())
    }

    /// Definition 31 `intercept(k, nu)`: `iota[k -> iota(k) +_k nu]` --
    /// a *derived* realization (no precondition), inheriting the
    /// provider table unchanged.
    pub fn intercept(&mut self, key: &str, nu: &str) {
        let kind = self.kind_of(key);
        let merged = kind.combine(&self.context_metadata(key), nu);
        self.context_carried.insert(key.to_string(), merged);
    }

    /// The merged metadata a `get(k, mu)` evaluation uses:
    /// `d(k) +_k iota(k)`, right-biased so `iota(k)` (the
    /// context-carried, enclosing-context value) takes priority over
    /// `mu` (the component-declared metadata `d(k)` passed in), per
    /// Definition 31's own text.
    pub fn resolve(&self, key: &str, component_declared: &str) -> String {
        let kind = self.kind_of(key);
        kind.combine(component_declared, &self.context_metadata(key))
    }
}
