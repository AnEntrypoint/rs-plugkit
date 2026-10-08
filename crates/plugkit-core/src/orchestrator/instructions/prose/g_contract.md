# contract is the only durable description

Gate `G_CONTRACT` on the spine. A gate is a condition that must hold before the phase advances; it is not a work phase.

Condition: contract is the only durable description. This is advisory: code does not refuse the transition on it, so carry it as a standing check on the work.

The walk is recorded by the transition that leaves this node.
