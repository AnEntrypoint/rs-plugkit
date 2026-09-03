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

## Decision 5: gm-native memory migration into the tencentdb_backend store

The user's migration-script ask has two distinct directions that were
conflated at first read:

1. **Importing an external, real TencentDB Agent Memory `vectors.db`** into
   gm -- this is what `tencentdb-compat-probe` (read-only) and
   `tencentdb_compat::read_l1_records`/`read_l0_conversations`/`read_skills`
   are for. Still read-only; a full write-side importer would translate rows
   into `tencentdb_memory::write_cfg` calls using the *source* embedding
   (whatever `embedding_meta` reports), not gm's own embedder -- not built
   yet because no concrete source vectors.db has been available to validate
   against this session, and building it against zero real data would be
   unverifiable narrative.
2. **Migrating gm's OWN native memories** (`.gm/memories/*.md`,
   `memory_md`/`rssearch_vectors`) into a project's `tencentdb_backend`
   store, so a project that later opts a namespace into the new backend
   doesn't lose what was already recalled through the old path. This is the
   `tencentdb-memory-import` verb (`wasm_dispatch/verbs.rs`): reads
   `memory_md::flat_kv_entries(source_namespace)`, re-embeds every doc
   through gm's own `embed::embed_text_json_passage` (fixed 384-dim), and
   writes via `tencentdb_memory::write_cfg` with an **explicit 384-dim
   config override** -- never the namespace's resolved
   `vectors_db_dims` (typically 768), which this content's embeddings
   cannot satisfy. Refuses if `dest_namespace` isn't actually listed in
   `memory.tencentdb_backend.namespaces`, so it can't silently write into an
   unrouted namespace's table.

## Session verification: real bugs found and fixed by an actual end-to-end run

Building `tencentdb-memory-import` and running it for real (release wasm
built and dispatched directly through `agentplug-runner`, not just
`cargo check`) surfaced two defects a compile-clean check could not catch:

1. **`flat_kv_entries`/`host_kv_query` is NOT the `.md` memory corpus.** It
   reads an unrelated discipline-KV JSON store
   (`.gm/disciplines/<ns>/*.json`, `agentplug-host`'s
   `kv_namespace_dir`) -- initial `tencentdb-memory-import` used it and
   silently imported zero docs even with real `.md` files present. Fixed by
   listing `memory_md::md_dir(ns)` via `pkfs::readdir` and parsing each
   `.md` file directly with `memory_md::parse`.
2. **A per-project (not per-namespace) `vectors_db_dims` makes single-table
   mixed-dim writes unsafe.** `ensure_schema`'s `CREATE TABLE IF NOT EXISTS`
   is a silent no-op once the table exists at its first-created width;
   writing gm's native 384-dim embeddings into a namespace already resolved
   at 768 either corrupts the fixed-width `F32_BLOB` column or (with a
   dim-suffixed table-name override, the first fix attempted) makes the
   import permanently unreachable through `recall`'s router, which always
   queries via the project-resolved (not import-specific) config. Fixed by
   refusing the import outright unless the destination namespace's
   resolved `vectors_db_dims` is actually 384 -- a project wanting both
   externally-embedded (768) and gm-native (384) tencentdb-routed content
   needs two separate namespaces, each configured at its own dim; there is
   no mixed-dim support within one namespace/table.

Verified live (scratch git repo, real release wasm built via
`cargo build --release --target wasm32-wasip1 --no-default-features
--features slim --lib`, dispatched with `agentplug-runner dispatch gm
<verb> <body>`): `memorize-fire` into `default` -> `tencentdb-memory-import`
into a `vectors_db_dims: 384`-configured namespace (1 doc imported) ->
`recall` against that namespace (real cosine-similarity hit, 0.845,
correct `file_path`, content lazily read from the pointer file, not stored
inline in the index row) -> `memorize-prune` (index row marked deleted,
file removed from disk). Full CRUD round trip confirmed against the actual
compiled artifact, not a mock or a narrated claim.

## Decision 6: L2 scene / L3 persona authority — file, not SQLite

Resolved by direct read of the vendored `MemoryCore` source
(`vendor/tencentdb-agent-memory`), not inference: neither L2 scenes nor L3
personas have a SQLite table at all.

- **L2 scenes**: `scene_blocks/*.md` files, META-delimited
  (`-----META-START-----`/`-----META-END-----` wrapping `created`/`updated`/
  `summary`/`heat`, then free-text content — `scene-format.ts::parseSceneBlock`).
  `.metadata/scene_index.json` is explicitly a rebuildable INDEX over these
  files, not an alternate content source: `scene-index.ts::syncSceneIndex`
  regenerates it by re-scanning `scene_blocks/`.
- **L3 persona**: a single `persona.md` file
  (`StoragePaths.persona = "persona.md"`, `core/storage/types.ts:250`) — one
  per data directory, no query/filter surface.

Grepped both modules for `sqlite`/`DatabaseSync`/`vectors_db`: zero hits in
either. Implemented as `tencentdb_compat::read_l2_scenes`/`read_l3_persona`,
reading files directly via `pkfs` (no `host_exec_js`/Node shell-out needed —
that mechanism stays reserved for the actual `node:sqlite` reads L0/L1/skills
require). `tencentdb-compat-probe` takes an optional `data_dir` body field
reporting `file_counts.{l2_scenes,l3_persona_exists}` alongside the existing
SQLite counts, independently of whether the SQLite probe itself succeeds —
live-verified by dispatching with a nonexistent `vectors_db_path` (SQLite
counts genuinely failed) and confirming `file_counts` still populated
correctly, proving the two data sources are properly decoupled.

## Open items tracked as their own PRD rows, not resolved here

- The prd.yml lost-update bug discovered mid-session (separate root cause,
  separate fix, tracked independently — not part of the memory-backend
  design itself, but found while planning it).
- The gm.db partial-sync starvation on the 892-file corpus (self-healing by
  design; only needs a fix if convergence proves too slow in practice,
  verified empirically, not assumed).
