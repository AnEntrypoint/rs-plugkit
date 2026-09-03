# CONC

YOU are the state machine. Plugkit does not audit in the background -- you run the checks and decide whether to `transition`.

Stage 5 of the pipeline: concurrency and hardware. Every pair of operations ordered by happens-before or provably disjoint; contention bounded as load grows; data laid out for the machine it runs on.

## Preferences (named, narrow)

Performance & Algorithmic Efficiency

* Big O Algorithmic Complexity (Donald Knuth)
* Data-Oriented Design (Mike Acton)
* Mechanical Sympathy (Martin Thompson)
* Zero-Cost Abstractions (Bjarne Stroustrup)
* Allocation Minimization & Cache Locality

## Sweeps

Every sweep is witnessed live, same turn -- a pass that depends on the interleaving you happened to get is not a pass.

**Happens-before.** Every new concurrent access is ordered by an explicit sync point (await, lock, channel, atomic, message boundary) or provably touches disjoint state. TOCTOU is the canonical violation: every single-instance/lock guard is atomic (O_EXCL, atomic rename, CAS), never check-then-act. Witness by interleaving the two calls under `exec_js` with a deterministic seed and asserting the outcome is ordering-invariant.

**Disjointness.** One writer per surface, concurrent writers backpressured to a defer queue. Where the diff shares a resource across async boundaries, name the owner; if you cannot name exactly one, restructure until you can.

**Contention.** No unbounded queue, no lock convoy, no retry storm: every wait has a bound, every retry a cap, every hot lock held for the shortest possible section. A path whose cost grows superlinearly with concurrent callers is a defect even when its measured mean is excellent -- bound the worst case, not the average.

**Machine fit.** The common access pattern is the one the layout is optimized for; the common case is the fall-through, not the jump. Profile to locate (`exec_js opts.profile:true`, browser `profile`/`trace` prefixes), then eliminate by live measurement, never intuit.

## Discovery

A fix shaped like a code repair routes `transition to=EMIT`. A fix shaped like a state-model change -- the ownership/disjointness boundary itself is wrong -- routes `transition to=STATE` (the graph's CONC -> STATE feedback edge, the contention-minimization loop). A discovery that reshapes the plan routes `transition to=SPECIFY`. Dispatch the transition; never narrate it.

## Dispatch

`transition to=SEC` only when every sweep above has a live witness behind it, same turn.
