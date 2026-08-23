/-!
Coeffect interception (paper Section 3.2.3, Definitions 30-31),
mirroring `orchestrator/coeffect_realm.rs`'s `InterceptionContext`/
`MergeKind`. Metadata values are fixed at `String` (the same reduction
`Isolation.lean` makes for `V_k`), and `M_k`'s monoid `(M_k, +_k,
epsilon_k)` is instantiated by two concrete, genuinely associative
operations matching `MergeKind`'s two Rust variants: right-biased
scalar overwrite, and (deduplicated, sorted) set union over
comma-separated tokens.
-/

namespace Coeffect

/-- A key's declared `(M_k, +_k, epsilon_k)` monoid shape (Definition
30's requirement that every key equip its metadata with a monoid),
matching `orchestrator/coeffect_realm.rs`'s `MergeKind` enum exactly. -/
inductive MergeKind where
  | scalarOverwrite
  | setUnion
  deriving DecidableEq, Repr

namespace MergeKind

/-- `epsilon_k`: both variants share the empty string as identity. -/
def identity : MergeKind → String := fun _ => ""

/-- The token-list normalization `setUnion` applies after appending:
sort, then remove adjacent-after-sort duplicates via `List.eraseDups`
(the `BEq`-based dedup this Lean core actually provides, unlike the
Mathlib-only `List.dedup`). -/
def normalize (xs : List String) : List String :=
  (xs.mergeSort (· ≤ ·)).eraseDups

/-- `+_k`, right-biased per Definition 31's own text ("this merge ...
is right-biased, so `iota(k)` takes priority and can override the
component's declaration"), matching `coeffect_realm.rs::MergeKind::
combine` field-for-field. -/
def combine : MergeKind → String → String → String
  | scalarOverwrite, l, r => if r = "" then l else r
  | setUnion, l, r =>
    let items := (l.splitOn ",").filter (· ≠ "") ++ (r.splitOn ",").filter (· ≠ "")
    String.intercalate "," (normalize items)

/-- `epsilon_k` is a left identity for `+_k` under `scalarOverwrite`:
`combine k "" r = r`, since the branch always returns `r` regardless of
`l` when `l = ""` unless `r = ""` too, in which case both sides are
`""`. States the identity law's LEFT half directly from the
definition's own `if`. -/
theorem scalarOverwrite_left_id (r : String) :
    MergeKind.scalarOverwrite.combine "" r = r := by
  unfold combine
  by_cases h : r = ""
  · simp [h]
  · simp [h]

/-- `epsilon_k` is a right identity for `+_k` under `scalarOverwrite`:
`combine k l "" = l`, directly from the `if r = ""` branch, which
always takes the `then` arm since the condition `"" = ""` decides
`true`. -/
theorem scalarOverwrite_right_id (l : String) :
    MergeKind.scalarOverwrite.combine l "" = l := by
  unfold combine
  simp

/-- `scalarOverwrite`'s `+_k` is associative: the right-biased overwrite
always settles on the rightmost non-empty operand (or `""` if all three
are empty), regardless of association -- a case split on whether each
of `r`, `s` is empty covers every branch both associations take. -/
theorem scalarOverwrite_assoc (l r s : String) :
    MergeKind.scalarOverwrite.combine (MergeKind.scalarOverwrite.combine l r) s
      = MergeKind.scalarOverwrite.combine l (MergeKind.scalarOverwrite.combine r s) := by
  unfold combine
  by_cases hs : s = ""
  · by_cases hr : r = "" <;> simp [hs, hr]
  · simp [hs]

/-- `setUnion`'s underlying token-list operation -- append, then
`normalize` (sort + `eraseDups`) -- is idempotent when the right
operand is already `[]`: appending nothing and re-normalizing an
already-normalized list is the identity. This is the token-list-level
content behind `setUnion`'s claimed `epsilon_k`-right-identity law,
stated over an arbitrary already-normalized token list `xs` rather
than re-deriving `""`'s own `splitOn` output, since that derivation is
string-library plumbing orthogonal to the merge law itself. -/
theorem setUnion_token_right_id (xs : List String) (hnorm : normalize xs = xs) :
    normalize (xs ++ ([] : List String)) = xs := by
  rw [List.append_nil]
  exact hnorm

end MergeKind

/-- Definition 30: `Sigma^inter := ((k:K) -> M_k) x ((k:K) -> (M_k -> V_k))`.
This file models only the `iota`/merge-kind half (`context_carried`,
`merge_kind`) -- the provider-function half `sigma : (k:K) -> (M_k ->
V_k)` is out of scope, matching `coeffect_realm.rs::InterceptionContext`
which likewise owns only `iota`'s merge machinery, never value storage. -/
structure Iota where
  contextCarried : List (String × String)
  mergeKind : List (String × MergeKind)
  deriving Repr

namespace Iota

def kindOf (t : Iota) (k : String) : MergeKind :=
  ((t.mergeKind.find? (fun p => p.1 == k)).map Prod.snd).getD MergeKind.scalarOverwrite

/-- `iota(k)`: `epsilon_k` when the key carries no context metadata,
matching `InterceptionContext::context_metadata`'s default. Reads the
LAST entry (via `find?` over the reversed list) for the same
reassignment reason `Isolation.lean`'s `SigmaIso.realmOf` does --
`intercept` below is a fold, not a once-only extension. -/
def contextMetadata (t : Iota) (k : String) : String :=
  ((t.contextCarried.reverse.find? (fun p => p.1 == k)).map Prod.snd).getD (t.kindOf k).identity

/-- Definition 31 `intercept(k, nu)`: `iota[k -> iota(k) +_k nu]` -- a
*derived* realization (no precondition), matching
`InterceptionContext::intercept` (merges the new value onto whatever
`contextMetadata` currently reports, then appends). -/
def intercept (t : Iota) (k nu : String) : Iota :=
  let merged := (t.kindOf k).combine (t.contextMetadata k) nu
  { t with contextCarried := t.contextCarried ++ [(k, merged)] }

/-- `intercept` never touches `mergeKind` -- matching Definition 31's
"the context that `intercept(k,v)` derives ... inherits the provider
table unchanged," carried here to the merge-kind declaration table
(the closest analogue this file's split of `Sigma^inter` has to that
provider table). -/
theorem intercept_preserves_mergeKind (t : Iota) (k nu : String) :
    (t.intercept k nu).mergeKind = t.mergeKind := rfl

/-- The merged metadata a `get(k, mu)` evaluation uses:
`d(k) +_k iota(k)`, right-biased so `iota(k)` (context-carried,
enclosing-context value) takes priority over `mu` (component-declared),
matching Definition 31's own text and `InterceptionContext::resolve`
exactly. -/
def resolve (t : Iota) (k componentDeclared : String) : String :=
  (t.kindOf k).combine componentDeclared (t.contextMetadata k)

/-- The right-bias property Definition 31 states explicitly: when the
key's context-carried metadata is non-empty (under `scalarOverwrite`),
`resolve` returns exactly that context-carried value, ignoring
whatever the component itself declared -- "letting an enclosing
context constrain how a component uses a coeffect without modifying
the component." -/
theorem resolve_right_biased_scalar (t : Iota) (k componentDeclared ctxVal : String)
    (hkind : t.kindOf k = MergeKind.scalarOverwrite)
    (hctx : t.contextMetadata k = ctxVal) (hne : ctxVal ≠ "") :
    t.resolve k componentDeclared = ctxVal := by
  unfold resolve
  rw [hkind, hctx]
  unfold MergeKind.combine
  simp [hne]

/-- When the enclosing context installs no interception for `k` at all
(`iota(k) = epsilon_k = ""`), `resolve` falls back to exactly the
component's own declared metadata under `scalarOverwrite` -- the
uninstalled-interception case reduces to plain component-declared
resolution, so `intercept` is genuinely optional per key. -/
theorem resolve_falls_back_to_component_scalar (t : Iota) (k componentDeclared : String)
    (hkind : t.kindOf k = MergeKind.scalarOverwrite)
    (hctx : t.contextMetadata k = "") :
    t.resolve k componentDeclared = componentDeclared := by
  unfold resolve
  rw [hkind, hctx]
  exact MergeKind.scalarOverwrite_right_id componentDeclared

/-- `intercept` for a key with no prior context metadata (fresh
interception, `scalarOverwrite`) installs exactly `nu` as the new
context-carried value -- the base case of the merge fold, matching
`combine`'s left-identity law applied through `intercept`'s own
definition. -/
theorem intercept_fresh_scalar (t : Iota) (k nu : String)
    (hkind : t.kindOf k = MergeKind.scalarOverwrite)
    (hctx : t.contextMetadata k = "") :
    (t.intercept k nu).contextMetadata k = nu := by
  have hmerged : (t.kindOf k).combine (t.contextMetadata k) nu = nu := by
    rw [hkind, hctx]
    exact MergeKind.scalarOverwrite_left_id nu
  have hkindEq : (t.intercept k nu).kindOf k = t.kindOf k := rfl
  unfold Iota.contextMetadata
  rw [hkindEq]
  unfold Iota.intercept
  rw [hmerged]
  simp only [List.reverse_append, List.reverse_cons, List.reverse_nil, List.nil_append,
    List.cons_append, List.nil_append, List.find?_cons]
  simp

end Iota

end Coeffect
