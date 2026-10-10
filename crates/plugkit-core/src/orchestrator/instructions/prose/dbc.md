# Design by Contract - Bertrand Meyer

Principle `DBC`, phase P2 CONTRACT. Apply "Design by Contract - Bertrand Meyer" to the work of the P2 CONTRACT phase.

Precondition: the PRD row this phase contracts has a subject and a witness, both readable with `prd-list {"id":"<row id>"}`.

Postcondition: the phase writes one precondition, one postcondition and one invariant, and each names the verb and the dispatch id whose live run falsifies it.

Invariant: every contract sentence names a verb that can run it live. A sentence that no live run can falsify is rewritten before the phase leaves.

The walk is recorded by the transition that leaves this node.
