# one task in flight

Gate `G_START` on the spine. A gate is a condition that must hold before the phase advances; it is not a work phase.

Condition: one task in flight. The transition out of this gate is refused until predicate `lean-one-task-in-flight` holds.

The walk is recorded by the transition that leaves this node.
