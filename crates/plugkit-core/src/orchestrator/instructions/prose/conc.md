# CONC

YOU are the state machine. Plugkit does not audit in the background -- you run the checks and decide whether to `transition`.

Stage 5 of the pipeline: concurrency and hardware. Every pair of operations ordered by happens-before or provably disjoint; contention bounded as load grows; data laid out for the machine it runs on.

## Happens-before sweep

For every new concurrent access in the diff: the two operations are ordered by an explicit sync point (await, lock, channel, atomic, message boundary) or provably touch disjoint state -- no shared mutable cell touched from two async paths without one. TOCTOU is the canonical violation: a check-then-act where atomic was required. Every single-instance/lock guard is atomic (O_EXCL, atomic rename, CAS), never read-then-write. Witness by interleaving the two calls under `exec_js` with a deterministic seed and asserting the outcome is ordering-invariant -- a pass that depends on the interleaving you happened to get is not a pass.

## Disjointness sweep

For every pair of writers: state sets disjoint (S_i intersect S_j = empty), and read sets disjoint where one side mutates. One writer per surface, concurrent writers backpressured to a defer queue -- a second writer racing the same file/row/connection is a defect in the shape, not bad luck. Where the diff shares a resource across async boundaries, name the owner; if you cannot name exactly one, restructure until you can.

## Contention sweep

No unbounded queue, no lock convoy, no retry storm: every wait has a bound, every retry a cap, every hot lock held for the shortest possible section. Contention dynamics: as n grows, contention goes to zero or stays flat -- a path whose cost grows superlinearly with concurrent callers (a global mutex around a long operation, a shared counter hammered per-request) is a defect even when its measured mean is excellent. Bound the worst case, not the average; the worst case is what a user experiences.

## Hardware-affinity sweep

Data laid out cache-local: flat spine (arrays, indices, contiguous fields) over pointer graphs; the common access pattern is the one the layout is optimized for. Branch-predictable hot paths: the common case is the fall-through, not the jump. Mutate in place, pools over allocation; no Promise chains / class hierarchies / operator overloading on hot paths. Benchmark before abstracting -- pass scope explicitly; closures hide scope cost in hot loops. Profile to locate (`exec_js opts.profile:true`, browser `profile`/`trace` prefixes), then eliminate by live measurement, never intuit.

## Discovery

A fix shaped like a code repair routes `transition to=EMIT`. A fix shaped like a state-model change -- the ownership/disjointness boundary itself is wrong -- routes `transition to=STATE` (the graph's CONC -> STATE feedback edge, the contention-minimization loop). A discovery that reshapes the plan routes `transition to=SPECIFY`. Dispatch the transition; never narrate it.

## Dispatch

`transition to=SEC` only when every sweep above has a live witness behind it, same turn.
