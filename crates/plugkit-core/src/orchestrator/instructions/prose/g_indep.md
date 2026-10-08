# verifier has not read the implementation

Gate `G_INDEP` on the spine. A gate is a condition that must hold before the phase advances; it is not a work phase.

Condition: verifier has not read the implementation. This is advisory: code does not refuse the transition on it, so carry it as a standing check on the work.

The walk is recorded by the transition that leaves this node.
