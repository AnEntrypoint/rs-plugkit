// Wiring witness for the degraded config-refresh signal. A refresh that did not finish cleanly
// has to reach config_resolve (Resolution::to_json) and instruction (the handle_instruction payload)
// as degraded plus a reason. Each link of that chain is checked in the source of crates/plugkit-core.
// Usage: node scripts/config-degraded-refresh-witness.mjs [--root=<rs-plugkit checkout>]
// Prints one line per link and ends with RESULT: PASS, or RESULT: FAIL naming the missing links.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const argv = Object.fromEntries(
  process.argv.slice(2).map((arg) => {
    const m = /^--([^=]+)=(.*)$/.exec(arg);
    return m ? [m[1], m[2]] : [arg, true];
  }),
);
const root = path.resolve(argv.root || path.join(scriptDir, '..'));
const srcDir = path.join(root, 'crates', 'plugkit-core', 'src');
const read = (rel) => fs.readFileSync(path.join(srcDir, rel), 'utf8');

const config = read('config.rs');
const configSync = read('config_sync.rs');
const configSyncNative = read('config_sync_native.rs');
const prose = read('prose.rs');
const instructions = read('orchestrator/instructions/mod.rs');

function resolveWithBody(text) {
  const start = text.indexOf('pub fn resolve_with(');
  const end = text.indexOf('fn load_implicit_default_repo_tier(');
  return start < 0 || end < start ? '' : text.slice(start, end);
}

const links = [
  ['RepoFetcher::refresh reports Freshness', config,
    (t) => /fn refresh\(&self, src: &RepoSource\) -> Result<Freshness, String>;/.test(t)],
  ['Freshness::Degraded carries a reason', config,
    (t) => /pub enum Freshness \{\s*Current,\s*Degraded \{ reason: String \},?\s*\}/.test(t)],
  ['Resolution carries the degraded reason', config,
    (t) => /pub degraded: Option<String>,/.test(t)],
  ['config_resolve reply names degraded and reason', config,
    (t) => /"degraded": self\.degraded\.is_some\(\),\s*"degraded_reason": self\.degraded,/.test(t)],
  ['repo-spec tiers record a degraded refresh', config,
    (t) => /Ok\(Freshness::Degraded \{ reason \}\) => \{\s*record_degraded\(degraded, reason\);/.test(t)],
  ['implicit tier records a degraded refresh', config,
    (t) => /Ok\(Freshness::Degraded \{ reason \}\) => record_degraded\(degraded, reason\),/.test(t)],
  ['implicit tier no longer discards the refresh result', config,
    (t) => !t.includes('let _ = fetcher.refresh(&src);')],
  ['every Resolution carries the accumulated degradation', config,
    (t) => {
      const body = resolveWithBody(t);
      const fields = body.match(/^\s*degraded,\s*$/gm) || [];
      return body.includes('let mut degraded: Option<String> = None;') && fields.length >= 5;
    }],
  ['GitRepoFetcher maps the degraded reason into Freshness', configSync,
    (t) => /Some\(reason\) => Freshness::Degraded \{ reason \}/.test(t)],
  ['GitRepoFetcher no longer discards the sync outcome', configSync,
    (t) => !/\.map\(\|_\| \(\)\)/.test(t)],
  ['publish reports an in-place checkout update as InPlace', configSync,
    (t) => /Publish::InPlace \{\s*reason: format!/.test(t)],
  ['refresh classifies an in-place update as degraded', configSync,
    (t) => /Publish::InPlace \{ reason \} =>\s*\{\s*st\.degraded_reason = Some\(reason\.clone\(\)\);/.test(t)],
  ['degraded refresh emits config_sync_degraded', configSync,
    (t) => /emit_degraded\(live\.clone\(\), reason, src\);/.test(t)],
  ['debounced sync outcomes keep the persisted reason', configSync,
    (t) => /degraded_reason: st\.degraded_reason\.clone\(\),\s*detail: format!\(\s*"debounced/.test(t)],
  ['native default cache returns the degraded reason', configSyncNative,
    (t) => /pub fn ensure_default_cache\(\) -> Result<\(String, Option<String>\), String>/.test(t)],
  ['native rename-aside refusal falls back to an in-place update', configSyncNative,
    (t) => /fn update_in_place\(/.test(t) && /Materialized::InPlace \{/.test(t)],
  ['prose names a degraded checkout as SourceRepoDegraded', prose,
    (t) => /SourceRepoDegraded \{ reason: String \}/.test(t) && /Ok\(\(cache, degraded\)\) =>/.test(t)],
  ['prose exposes the degraded reason to the caller', prose,
    (t) => /pub fn resolve_with_degradation\(/.test(t)],
  ['instruction reply names degraded and reason', instructions,
    (t) => /"degraded": instruction_degraded\.is_some\(\),\s*"degraded_reason": instruction_degraded,/.test(t)],
  ['instruction prose is served through the degradation-aware path', instructions,
    (t) => /resolve_with_degradation\(&key/.test(t)],
];

const missing = [];
for (const [name, text, holds] of links) {
  const pass = holds(text);
  console.log(`${pass ? 'ok     ' : 'MISSING'} ${name}`);
  if (!pass) missing.push(name);
}
if (missing.length === 0) {
  console.log(`RESULT: PASS ${links.length} links in ${root}`);
} else {
  console.log(`RESULT: FAIL ${missing.length} missing in ${root}: ${missing.join('; ')}`);
  process.exitCode = 1;
}
