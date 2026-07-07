---
key: mem-7fa64fdece8b86a1-731
ns: default
created: 1783349428298
updated: 1783349428298
---

## Resolved mutable: vecmig-discipline-fanout

Decision: same deferral as vecmig-recency-dedup-semantics -- read path is not cutting over today because of the in-memory-DB blocker (shared-db-inmemory-not-persisted), so no fan-out replication is needed yet. Recorded for the eventual cutover: the write path this session DOES add (dual-write into rssearch_vectors alongside the existing host_kv_put(${ns}-vec) call) must write with the real caller-supplied namespace column so a future read-path migration can replicate enabledDisciplineNamespaces(namespace) fan-out via a SQL `WHERE namespace IN (?, ?, ...)` built from the same .gm/disciplines/enabled.txt read rs-plugkit already does elsewhere, rather than a UNION per namespace.
