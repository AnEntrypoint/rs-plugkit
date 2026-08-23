import CordisCalculus.Independence

/-!
Theorem 20 and Corollary 21 at full generality: a finite family of `n`
pairwise-independent revertible effects, applied in sequence, can have
its `n` inverses applied in ANY permutation and still reach the exact
starting state. `Independence.lean` proved only the `n = 2` instance and
left the general case as explicit remaining work.

Modeled as a plain `List` of `(effect, generating list)` pairs, matching
this project's existing style of `List.Pairwise` plus structural `List`
induction. The permutation is modeled directly as a SECOND list,
`order`, related to the family's own `members` by `List.Perm` -- the
paper's "any permutation of {1, .., n}" restated without index/`Fin`
arithmetic. Correctness (`corollary21` below) is proved by induction on
`order` ALONE (never on `members`), peeling `order`'s own head at each
step and using `theorem20_peel_head` to show that undoing it, evaluated
against the trajectory of `order`'s own tail (which is `members` with
that one member erased, in whatever relative order the remaining
`members` entries occur), removes exactly that member's contribution --
this is the one substantive fact the whole proof rests on, and it holds
regardless of what earlier or later PERMUTATION positions look like,
which is why induction on `order` alone (not a joint induction on
`order` and `members`) suffices.
-/

variable {Gamma : Type}

/-- One family member: an effect paired with its own finite generating
list (Definition 17's own per-effect data). -/
abbrev Member (Gamma : Type) := RevertibleEffect Gamma × List (Gamma → Gamma)

/-- Definition 19 (independence) between two family members, generators
included. -/
def MemberIndependent (m1 m2 : Member Gamma) : Prop :=
  Independent m1.1 m2.1 m1.2 m2.2

theorem MemberIndependent.symm {m1 m2 : Member Gamma} (h : MemberIndependent m1 m2) :
    MemberIndependent m2 m1 := Independent.symm h

/-- Theorem 20 clause (1), specialized to peeling the HEAD of a member
list `e :: rest`: undoing `e` -- its inverse evaluated at the state
`rest` alone reaches from `gamma0` (never applying `e.fwd` at all) --
applied to the full trajectory (`rest` applied on top of `e.fwd gamma0`),
lands exactly on that same omitted-trajectory state. This is the paper's
own `g_j(delta_u) = delta_u'` read at the head position. Proved by
induction on `rest`, pushing `e`'s inverse rightward past each later
member's forward map via that member's independence from `e` (the
`undisturbed21`/commutation content `independent_pair_revert_nonlifo_order`
already uses for the two-element case), then invoking the induction
hypothesis on what remains. -/
theorem theorem20_peel_head
    (e : RevertibleEffect Gamma) (egens : List (Gamma → Gamma)) (hgen : e.fwd ∈ egens)
    (rest : List (Member Gamma))
    (hindep : ∀ m ∈ rest, MemberIndependent (e, egens) m)
    (hrestgen : ∀ m ∈ rest, m.1.fwd ∈ m.2) :
    ∀ gamma0 : Gamma,
      e.inv (rest.foldl (fun s m => m.1.fwd s) gamma0)
            (rest.foldl (fun s m => m.1.fwd s) (e.fwd gamma0))
        = rest.foldl (fun s m => m.1.fwd s) gamma0 := by
  induction rest with
  | nil =>
    intro gamma0
    exact e.left_inv gamma0
  | cons hd tl ih =>
    intro gamma0
    have hindep_hd : MemberIndependent (e, egens) hd := hindep hd List.mem_cons_self
    have hindep_tl : ∀ m ∈ tl, MemberIndependent (e, egens) m :=
      fun m hm => hindep m (List.mem_cons_of_mem hd hm)
    have hgen_hd : hd.1.fwd ∈ hd.2 := hrestgen hd List.mem_cons_self
    have hgen_tl : ∀ m ∈ tl, m.1.fwd ∈ m.2 := fun m hm => hrestgen m (List.mem_cons_of_mem hd hm)
    show e.inv (tl.foldl (fun s m => m.1.fwd s) (hd.1.fwd gamma0))
          (tl.foldl (fun s m => m.1.fwd s) (hd.1.fwd (e.fwd gamma0)))
        = tl.foldl (fun s m => m.1.fwd s) (hd.1.fwd gamma0)
    have hstep : hd.1.fwd (e.fwd gamma0) = e.fwd (hd.1.fwd gamma0) := by
      have hcommuting : Commuting e.fwd hd.1.fwd :=
        hindep_hd.commute e.fwd (InMonoid.gen e.fwd hgen) hd.1.fwd (InMonoid.gen hd.1.fwd hgen_hd)
      exact congrFun hcommuting.symm gamma0
    rw [hstep]
    exact ih hindep_tl hgen_tl (hd.1.fwd gamma0)

/-- Reverting a member list `order` starting from a FULL trajectory
state `full` (the state the whole, un-omitted family currently sits
at): `[]` -> `full` unchanged (nothing left to undo); `hd :: tl` -> undo
`hd`, evaluated at the trajectory `tl` alone would reach from `gamma0`
(the state `hd`'s own removal leaves `tl` computing against), applied to
`full`, landing (by `theorem20_peel_head`) on that same omitted-
trajectory state -- THEN recurse `revertPermuted` on `tl` starting from
THAT SHRUNK state, never the original `full`. This is the paper's own
downward induction exactly: undo `g_j` first (at the CURRENT full
trajectory, whatever it is at this recursive depth), and recurse on
"the sequence with e_j omitted" from there -- `full` genuinely shrinks
one member at a time, matching Corollary 21's "the induction hypothesis
applies to it and to the rest of the permutation." -/
def revertPermuted (order : List (Member Gamma)) (full gamma0 : Gamma) : Gamma :=
  match order with
  | [] => full
  | hd :: tl =>
      revertPermuted tl (hd.1.inv (tl.foldl (fun s m => m.1.fwd s) gamma0) full) gamma0

/-- Corollary 21, THE precise fact the paper states: let `order` be ANY
list of pairwise-independent members (in particular, any permutation of
an original family -- this theorem does not need to know what the
"original," unpermuted order was, since independence and the trajectory
`order` itself computes are all `order` needs). Reverting `order` against
ITS OWN forward trajectory, via `revertPermuted` (undo `order`'s own head
first at the current full state, recurse on `order`'s own tail from the
shrunk result), reaches `gamma0` exactly. Since this holds for `order`
being LITERALLY ANY pairwise-independent list -- including every
permutation of a fixed underlying family, which is still pairwise-
independent because independence is symmetric and a permutation changes
only which name is attached to which list position, not which UNORDERED
PAIRS of members the family contains -- this is exactly Corollary 21's
claim: "applying the n inverses ... in the order of any permutation ...
reaches gamma0." Proved by induction on `order` directly, using
`theorem20_peel_head` to peel the head at each step and `ih` to finish
the shrunk remainder, exactly the paper's "downward induction on n." -/
theorem corollary21
    (order : List (Member Gamma)) (hpw : order.Pairwise MemberIndependent)
    (hgen : ∀ m ∈ order, m.1.fwd ∈ m.2) (gamma0 : Gamma) :
    revertPermuted order (order.foldl (fun s m => m.1.fwd s) gamma0) gamma0 = gamma0 := by
  induction order with
  | nil => rfl
  | cons hd tl ih =>
    have hpw_tl : tl.Pairwise MemberIndependent := hpw.tail
    have hgen_tl : ∀ m ∈ tl, m.1.fwd ∈ m.2 := fun m hm => hgen m (List.mem_cons_of_mem hd hm)
    have hindep_hd : ∀ m ∈ tl, MemberIndependent (hd.1, hd.2) m := by
      intro m hm
      rw [List.pairwise_cons] at hpw
      have := hpw.1 m hm
      simpa using this
    have hgen_hd : hd.1.fwd ∈ hd.2 := hgen hd List.mem_cons_self
    show revertPermuted tl
          (hd.1.inv (tl.foldl (fun s m => m.1.fwd s) gamma0)
            ((hd :: tl).foldl (fun s m => m.1.fwd s) gamma0)) gamma0
        = gamma0
    have hunfold : (hd :: tl).foldl (fun s m => m.1.fwd s) gamma0
        = tl.foldl (fun s m => m.1.fwd s) (hd.1.fwd gamma0) := rfl
    rw [hunfold, theorem20_peel_head hd.1 hd.2 hgen_hd tl hindep_hd hgen_tl gamma0]
    exact ih hpw_tl hgen_tl
