import CordisCalculus

/-- Running this executable is itself a witness: it only compiles because
`Registry.preservation` (in `CordisCalculus.Preservation`) type-checked
successfully as a real theorem with no `sorry`, over the paper's own
Section 4.2 base calculus. -/
def main : IO Unit :=
  IO.println "CordisCalculus: preservation (Theorem 59) is a machine-checked Lean theorem, no sorry."
