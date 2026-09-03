import CordisCalculus.Basic

/-!
Paper Section 4.3: the layer the base calculus of `Basic.lean` explicitly
drops. `Basic.lean` collapses a whole transition into one atomic step, so
its `LifecycleState` has only `inactive`/`active`. Definition 49 replaces
that two-point space by four states, two of which are transitions in
progress:

  Theta := Inactive(zeta) | Reloading(i, g, omega) | Active(g, omega)
         | Unloading(g, omega, zeta)

where `i` is the remaining effect iterator (Definition 51), `g` the
accumulator built so far, `omega` the committed view, and `zeta` the
outcome. Table 1 of Section 4.4 reads the ten rules of Section 4.3 as
writes on the fiber they act on; `Step` below is that table, and
`Trace` is a sequence of such steps starting from the empty registry
Definition 53 requires.

The abstract context `Γ` of the paper is left as a type parameter here:
every result proved below is a statement about the control-field
structure of the lifecycle and the accumulator algebra over it, quantified
over EVERY choice of context type, so nothing is specialised to gm's own
registry-of-strings instance.
-/

universe u

namespace Cordis

/-- Paper Definition 51's `E_Gamma^iter`: a coinductive-in-spirit iterator
whose every iteration yields a new context, an inverse for the effect it
just performed, and a `Maybe` continuation. Lean's positivity checker
admits the recursive occurrence under the function arrow's codomain, so
this is a genuine inductive type rather than an axiomatised one -- an
iterator here is therefore always finite-depth, which is exactly the
`len(e_n) <= K` hypothesis Theorem 66 assumes of every effect function. -/
inductive EffectIter (Γ : Type u) : Type u where
  | done (run : Γ → Γ × (Γ → Γ))
  | more (run : Γ → Γ × (Γ → Γ)) (next : EffectIter Γ)

namespace EffectIter

variable {Γ : Type u}

/-- The context an iteration produces, `pr1 ∘ i` in Table 1's `Ψ` column. -/
def stateMap : EffectIter Γ → Γ → Γ
  | .done run, γ => (run γ).1
  | .more run _, γ => (run γ).1

/-- The inverse an iteration yields, `h` in Table 1's third column. -/
def inverse : EffectIter Γ → Γ → (Γ → Γ)
  | .done run, γ => (run γ).2
  | .more run _, γ => (run γ).2

/-- The continuation: `Nothing` at `done`, `Just i'` at `more`. -/
def continuation : EffectIter Γ → Option (EffectIter Γ)
  | .done _ => none
  | .more _ next => some next

/-- The number of iterations remaining, the paper's `len(e_n)`. -/
def length : EffectIter Γ → Nat
  | .done _ => 1
  | .more _ next => next.length + 1

theorem length_pos (i : EffectIter Γ) : 0 < i.length := by
  cases i <;> simp [length]

/-- Definition 51's witness condition, the `E_Gamma^iter*` refinement:
each iteration's yielded inverse really does undo the context change that
iteration made. Read on the nose (with `≃` taken as equality on `Γ`), the
reading Definition 51's own last sentence names. -/
def Witnessed : EffectIter Γ → Prop
  | .done run => ∀ γ, (run γ).2 ((run γ).1) = γ
  | .more run next => (∀ γ, (run γ).2 ((run γ).1) = γ) ∧ next.Witnessed

theorem Witnessed.head {i : EffectIter Γ} (h : i.Witnessed) (γ : Γ) :
    i.inverse γ (i.stateMap γ) = γ := by
  cases i with
  | done run => exact h γ
  | more run next => exact h.1 γ

theorem Witnessed.tail {run : Γ → Γ × (Γ → Γ)} {next : EffectIter Γ}
    (h : (EffectIter.more run next).Witnessed) : next.Witnessed := h.2

end EffectIter

/-- Paper Definition 49, equation (43): the four lifecycle states, with
`Reloading` and `Unloading` present as real inhabitants rather than
collapsed into an atomic step. `outcome` is `{⊥} ∪ Ξ`, modelled as
`Option Error` with `none` for `⊥`.

The paper's `omega : d → N` (the committed view) is modelled as a total
map from the key type to a name type; nothing below inspects its values
beyond equality, so it is left as an arbitrary function type. -/
inductive Lifecycle (Γ : Type u) (View : Type u) (Error : Type u) : Type u where
  | inactive (outcome : Option Error)
  | reloading (remaining : EffectIter Γ) (accum : Γ → Γ) (view : View)
  | active (accum : Γ → Γ) (view : View)
  | unloading (accum : Γ → Γ) (view : View) (outcome : Option Error)

namespace Lifecycle

variable {Γ : Type u} {View : Type u} {Error : Type u}

/-- Definition 49, equation (44): a fiber is installed in each of the
three states carrying an accumulator and a committed view. -/
def installed : Lifecycle Γ View Error → Prop
  | .inactive _ => False
  | _ => True

/-- Definition 49, equation (44): a fiber is failed when its `Inactive`
outcome carries an error. -/
def failed : Lifecycle Γ View Error → Prop
  | .inactive (some _) => True
  | _ => False

/-- The accumulator an installed state carries; `id` where the state
carries none, which the `installed` hypothesis of every result below
excludes from ever being read. -/
def accumulator : Lifecycle Γ View Error → (Γ → Γ)
  | .inactive _ => id
  | .reloading _ g _ => g
  | .active g _ => g
  | .unloading g _ _ => g

/-- The committed view an installed state carries. Definition 49's
"an installed fiber `n` resolves `k` to `m` when `omega_n(k) = m`" reads
this field. -/
def committedView : Lifecycle Γ View Error → Option View
  | .inactive _ => none
  | .reloading _ _ ω => some ω
  | .active _ ω => some ω
  | .unloading _ ω _ => some ω

theorem committedView_isSome_of_installed {θ : Lifecycle Γ View Error}
    (h : θ.installed) : (θ.committedView).isSome := by
  cases θ <;> simp_all [installed, committedView]

/-- `sigma_gamma` unions the tables of `Active` fibers alone (Definition
49's second reading), so a fiber whose transition is under way in either
direction provides none of its own coeffects. This predicate is what a
coeffect context is built over. -/
def providing : Lifecycle Γ View Error → Prop
  | .active _ _ => True
  | _ => False

theorem installed_of_providing {θ : Lifecycle Γ View Error}
    (h : θ.providing) : θ.installed := by
  cases θ <;> simp_all [providing, installed]

/-- `Reloading` is precisely where an iteration may run. -/
def inFlight : Lifecycle Γ View Error → Prop
  | .reloading _ _ _ => True
  | _ => False

end Lifecycle

end Cordis
