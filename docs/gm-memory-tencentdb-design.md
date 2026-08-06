# gm ↔ TencentDB Agent Memory: architecture decision record

Decision record for the memory-backend replacement work tracked in `.gm/prd.yml`
(rows added under the `gm-memory-tencentdb` initiative). Not a substitute for the
implementation itself — this records the decisions made and why, so later
sessions don't re-litigate them.

## Current state (as of this session)

gm's memory system is compiled Rust in `crates/plugkit-core/src/`:

- Verbs: `memorize`, `memorize-fire`, `recall`, `auto_recall`, `memorize-prune`
  (`wasm_dispatch/verbs.rs`, `orchestrator/memorize.rs`, `orchestrator/recall.rs`).
- Storage: raw markdown files at `.gm/memories/<key>.md` for the `default`
  namespace (`.gm/disciplines/<ns>/memories/<key>.md` otherwise), key format
  `mem-{fnv1a64(ns|text):016x}-{text.len()}` (`memory_md.rs`). The `.md` corpus
  is the durable store — code comments say so explicitly, and `memorize`
  refuses to persist a memory whose file write failed.
- Index/cache: `.gm/gm.db` (libsql), table `rssearch_vectors`
  `(namespace, key, text, embedding F32_BLOB(384), updated_at, deleted)`.
  **This table duplicates full text inline**, not a pure pointer — a query
  convenience the current design deliberately trades for pointer purity.
- Embedding: `BAAI/bge-small-en-v1.5` baked into the wasm binary, fixed
  384-dim, compile-time-asserted against `vecstore::EXPECTED_EMBED_DIM`
  (`embed.rs`).
- Sync: lazy, budget-limited (2000ms/1500ms per pass), self-healing across
  multiple dispatches via a `:partial=N` digest marker
  (`memory_md.rs::sync_index`) — already fixed once for a prior "tracked but
  never embedded" starvation bug; the same class can recur on a large corpus.

## Decision 1: layered storage, not a single format

**Question**: the request asked for both "raw files with cached indexing that
point to file locations, not contain contents" AND "100% format-compatible
with the original Tencent project" — Tencent's own format stores content
inline in SQLite rows (`l1_records.content`, `l0_conversations.message_text`,
`skills.content`), which directly contradicts pointer-only storage.

**Resolved** (user confirmed, layered approach): gm's **native** memory
storage stays file-pointer + lean cache index — this is what the new
`tencentdb_memory` module and its `gm.db` table implement, and it is *not*
required to match Tencent's schema. A **separate compat module** targets
Tencent's exact `vectors.db`/`metadata.db` schema, used only for migration
and interop with real TencentDB Agent Memory tooling. The two never share a
table or a dimension.

## Decision 2: gm-config's role

`gm-config` (submodule, `../gm/gm-config`) is prose/FSM-graph/gate-message/
config-value orchestration only — confirmed via its own README ("This repo
is the source, not a snapshot") and its directory layout (`prose/`, `fsm/`,
`gates/`, `residual/`, `hooks/`, `gm.config.json`). It holds no data and has
no existing "stateful plugin" pattern beyond a narrow `:memory:`+name
`HashMap<String, RawDb>` registry in `code_index.rs`, scoped specifically to
hosts with no real filesystem (browser).

"Orchestrate with gm-config to make stateless operations stateful" is
implemented as: extend `gm.config.json`'s existing `memory.*` block with a
`tencentdb_backend` sub-block (enabled flag, data dir, dims, namespace
routing, sync budgets) — this makes backend selection and tuning
config-driven rather than hardcoded. The verbs themselves stay
stateless-per-call (open the db file, do one operation, close), matching how
`agentplug-libsql`/`agentplug-bert` already work — this codebase has no
precedent for genuine in-process session state beyond the narrow exception
above, and this design does not invent one without a proven need.

## Decision 3: embedding dimension mismatch

gm's embedder is fixed at 384-dim (compiled model). TencentDB Agent Memory's
local embedding is provider-configurable — 768-dim for the default local
`embeddinggemma-300m` model, or an OpenAI-configured dimension when using a
remote provider. These cannot share a vector column.

**Resolved**: the new `tencentdb_memory` index table stores its embedding
column at a **configurable dimension** (`memory.tencentdb_backend.
vectors_db_dims` in `gm.config.json`, default 768), separate from the
existing 384-dim `rssearch_vectors` table. A migration re-embeds content
through gm's own bge-small-en-v1.5 pipeline when writing into gm's native
store (dimension-safe, loses original embedding provenance — acceptable
since migrated content was never Tencent-native-embedded through gm's
pipeline in the first place). The compat module, when materializing
Tencent's actual schema, uses whatever dimension the source system's
`embedding_meta` fingerprint row specifies.

## Decision 4: what NOT to change

- `codesearch`/`code_chunks` indexing is unrelated to this work and is not
  touched — it already indexes directly into `gm.db` rows (no separate file
  corpus) and that's out of scope here.
- The existing `memorize`/`recall`/`memorize-fire`/`memorize-prune` verb
  *surface* (function names, request/response shape) does not change for
  existing callers. Backend selection is transparent and config-gated,
  default-disabled, so unmodified projects see zero behavior change.

## Open items tracked as their own PRD rows, not resolved here

- The prd.yml lost-update bug discovered mid-session (separate root cause,
  separate fix, tracked independently — not part of the memory-backend
  design itself, but found while planning it).
- The gm.db partial-sync starvation on the 892-file corpus (self-healing by
  design; only needs a fix if convergence proves too slow in practice,
  verified empirically, not assumed).
- Whether L2 (scene) and L3 (persona) TencentDB content is authoritative in
  the SQLite row or the on-disk `.md` file it also writes — must be resolved
  before the compat module's schema is finalized for those two asset kinds.
