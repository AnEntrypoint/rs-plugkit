---
key: mem-d9cfc3986baf1b1a-532
ns: default
created: 1783234062110
updated: 1783234062110
---

## Resolved mutable: mut-1783169004652

prd-add id-param mismatch traced to a stale watcher pinned at 0.1.753 during a prior session while 0.1.756+ was already published upstream fixing prd-add id-collision handling; current session's watcher runs 0.1.760 and prd-add/prd-resolve calls in this session (fix-gm-metadata-bump-missing-gm-plugkit-version etc, resolved this session) worked correctly against explicit ids with no id-param-ignored recurrence -- confirms fixed upstream by version bump, no rs-plugkit source change needed.
