# contract satisfied and recorded

Gate `G_DONE` on the spine. A gate is a condition that must hold before the phase advances; it is not a work phase.

Condition: contract satisfied and recorded. The transition out of this gate is refused until predicate `lean-contract-recorded` holds.

The walk is recorded by the transition that leaves this node.
