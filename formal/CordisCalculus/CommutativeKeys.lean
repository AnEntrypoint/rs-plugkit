import CordisCalculus.Independence

/-!
Commutative keys (paper Section 3.2.3-adjacent material threaded through
Section 3.1.3's numbering: Definition 39, Theorem 40, Theorem 42). This
file is the connective piece `Independence.lean`'s own header flagged as
explicit remaining work: it specializes that file's generic
`Commuting`/`InMonoid`/`Independent` vocabulary to the paper's actual
motivating case -- a coeffect context keyed by `String`, where an
operation only ever reads/writes ONE key, `sigma[k -> u(sigma[k])]` --
and proves Theorem 40 (operations at distinct keys are independent)
unconditionally, plus states Theorem 42 (operations built from a
commutative key are independent too) as the paper states it: NOT a
theorem this file proves outright, since the paper's own proof of
Theorem 42 inducts on `Definition 41`'s coeffect-mediated effect
functions, an inductive family this codebase's abstract `Gamma`-level
`Independence.lean` deliberately has no counterpart for (adding
Definition 41's own inductive type would be importing a THIRD paper
framework, not closing this one) -- Theorem 42 is instead stated as a
hypothesis-carrying theorem taking generator-commutativity as a
PREMISE, which is exactly the content the paper's own induction reduces
to before it ever touches Definition 41 (see the paper's proof of
Theorem 42, clause 1: "it is therefore enough ... that a generator of an
operation occurring in e1 commute with a generator of one occurring in
e2 ... this is Theorem 40, and where they lie at one key that key
carries operations of both and is commutative by hypothesis").
-/

variable {V : Type}

/-- A coeffect context: a total map from `String` keys to values of type
`V` (paper's `sigma : Sigma`, `Sigma = (k : K) -> V_k` specialized to one
value type per this file, since the dependent-value-type generality
Definition 22 states is not needed to prove Theorem 40's own content:
distinct-key generators commute regardless of whether the two keys'
value types agree). -/
abbrev CoeffectCtx (V : Type) := String → V

/-- An operation's lift at one key `k` (paper's `a_Sigma(x)` restricted
to its state-transforming component, `pr3` elided the same way
`Independence.lean`'s header elides Definition 17's effect-function
output down to its forward/inverse pair): reads and writes ONLY `sigma
k`, leaving every other key exactly as it stood. This is Definition 24's
generator shape stated directly, the premise Theorem 40's own paper
proof invokes ("every generator of M(a_Sigma) is of the form sigma |->
sigma[k -> u(sigma(k))]"). -/
def KeyOp (k : String) (u : V → V) : CoeffectCtx V → CoeffectCtx V :=
  fun sigma k' => if k' == k then u (sigma k) else sigma k'

/-- Two `KeyOp` generators at DIFFERENT keys commute unconditionally --
each only ever touches its own key's slot, so applying one then the
other (in either order) writes both slots to the same final values. -/
theorem keyOp_commuting_of_ne {k1 k2 : String} (hne : k1 ≠ k2) (u1 u2 : V → V) :
    Commuting (KeyOp k1 u1) (KeyOp k2 u2) := by
  have hk12 : (k1 == k2) = false := by simp only [beq_eq_false_iff_ne, ne_eq]; exact hne
  have hk21 : (k2 == k1) = false := by simp only [beq_eq_false_iff_ne, ne_eq]; exact Ne.symm hne
  funext sigma
  funext k'
  show (KeyOp k1 u1) ((KeyOp k2 u2) sigma) k' = (KeyOp k2 u2) ((KeyOp k1 u1) sigma) k'
  unfold KeyOp
  by_cases h1 : k' = k1
  · subst h1
    rw [if_pos (beq_self_eq_true k'), if_neg (by simpa using beq_eq_false_iff_ne.mpr hne),
        if_pos (beq_self_eq_true k'), hk12]
    simp
  · by_cases h2 : k' = k2
    · subst h2
      rw [if_neg (by simpa using beq_eq_false_iff_ne.mpr (Ne.symm hne)),
          if_pos (beq_self_eq_true k'), if_pos (beq_self_eq_true k'), hk21]
      simp
    · rw [if_neg (by simpa using beq_eq_false_iff_ne.mpr h1),
          if_neg (by simpa using beq_eq_false_iff_ne.mpr h2),
          if_neg (by simpa using beq_eq_false_iff_ne.mpr h2),
          if_neg (by simpa using beq_eq_false_iff_ne.mpr h1)]

/-- Theorem 40 (distinct keys are independent), the commutation clause:
a whole FAMILY of `KeyOp` generators at `k1`, closed under `InMonoid`,
commutes with a whole family of `KeyOp` generators at a different `k2`
-- `commuting_of_generators_commuting` (Lemma 18, `Independence.lean`)
lifts the pairwise generator fact above from generators to the full
transformation monoids, exactly as the paper's own proof does ("Lemma
18(1) extends the commutation from the generators to the two
monoids"). -/
theorem theorem40_commute
    {k1 k2 : String} (hne : k1 ≠ k2)
    (us1 us2 : List (V → V)) :
    ∀ f1, InMonoid (us1.map (KeyOp k1)) f1 →
    ∀ f2, InMonoid (us2.map (KeyOp k2)) f2 →
    Commuting f1 f2 := by
  apply commuting_of_generators_commuting
  intro g1 hg1 g2 hg2
  obtain ⟨u1, _, heq1⟩ := List.mem_map.mp hg1
  obtain ⟨u2, _, heq2⟩ := List.mem_map.mp hg2
  rw [← heq1, ← heq2]
  exact keyOp_commuting_of_ne hne u1 u2

/-- The paper's second Theorem-40 clause: what a `KeyOp k1` generator
YIELDS (the value now sitting at `k1`) is left untouched by any `KeyOp
k2` generator at a different key -- "what a_Sigma yields at sigma ...
is determined by sigma(k), which every generator of M(a'_Sigma) leaves
as it stands." This is `InverseUndisturbed`'s content specialized to
`KeyOp`: reading key `k1` after applying an arbitrary `k2`-generator
(`k1 != k2`) agrees with reading `k1` before. -/
theorem keyOp_reads_undisturbed_at_distinct_key
    {k1 k2 : String} (hne : k1 ≠ k2) (u2 : V → V) (sigma : CoeffectCtx V) :
    (KeyOp k2 u2 sigma) k1 = sigma k1 := by
  unfold KeyOp
  have h : (k1 == k2) = false := by
    simp only [beq_eq_false_iff_ne, ne_eq]
    exact hne
  simp only [h, Bool.false_eq_true, if_false]

/-- Theorem 42, stated as the paper's own proof reduces it (see this
file's header): given that every generator of the operations occurring
in `e1` commutes with every generator of those occurring in `e2` --
which Theorem 40 above discharges outright for any two operations at
distinct keys, and which a commutative key (Definition 39) discharges
by hypothesis for two operations sharing one key -- `e1` and `e2` are
independent (Definition 19), PROVIDED both effects' generating lists
also individually satisfy `InverseUndisturbed` against the other. This
codebase does not model Definition 41's inductive coeffect-mediated
effect-function family (see header), so `e1`/`e2` here are literal
`RevertibleEffect`s with caller-supplied generating lists rather than
values freshly built by that induction; the paper's own inductive
argument is exactly what would discharge the `InverseUndisturbed`
premises automatically for an `e1`/`e2` built via Definition 41, the
one piece of Theorem 42's proof this file does not re-derive. -/
theorem theorem42_of_generator_commutation
    {Gamma : Type} (e1 e2 : RevertibleEffect Gamma) (gens1 gens2 : List (Gamma → Gamma))
    (hcomm : ∀ g1 ∈ gens1, ∀ g2 ∈ gens2, Commuting g1 g2)
    (hu12 : InverseUndisturbed gens1 e2) (hu21 : InverseUndisturbed gens2 e1) :
    Independent e1 e2 gens1 gens2 :=
  independent_of_generators e1 e2 gens1 gens2 hcomm hu12 hu21
