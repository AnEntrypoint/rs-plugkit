# Agentic Property-Based Testing - Hypothesis agent study

Principle `PBTAGENT`, phase P4 VERIFY. Apply "Agentic Property-Based Testing - Hypothesis agent study" to the work of the P4 VERIFY phase.

The agent proposes candidate properties from the PRD rows and admits only those statable from the contract alone. A candidate derivable from the function body is rejected, and the rejection with its reason goes into the row's witness text. Admitted properties run through the HUGHES probe. A property that restates the code fires the backreference to METAMORPH, and the agent states a relation between outputs instead.

Live witness for the admission rule: one exec_js script takes a candidate property and its source function, reads the function body, and prints `admit` when the property is stated from the contract alone or `reject: <reason>` when the body derives it. The expected line is recorded in the row's witness text before the dispatch runs.

The walk is recorded by the transition that leaves this node.
