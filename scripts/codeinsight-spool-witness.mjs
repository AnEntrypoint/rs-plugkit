// Live-spool witness for the code index. Over one window of the spool log it requires: zero
// code_index_treesitter_failed events, every such event attributed to a file (path + plugin_failure),
// at most one codeinsight_rebuild, and every rebuild naming the changed files that forced it (no bare
// stored-then-current digest pair).
// Usage: node scripts/codeinsight-spool-witness.mjs [--log <watcher.log>] [--window-ms 600000] [--window-end-ms <ms>]
import { readFileSync } from 'node:fs';

const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : fallback;
};
const logPath = opt('--log', 'C:/dev/spoint/.gm/exec-spool/.watcher.log');
const windowMs = Number(opt('--window-ms', '600000'));

const events = [];
for (const line of readFileSync(logPath, 'utf8').split('\n')) {
  const m = line.match(/evt:\s*(\{.*\})\s*$/);
  if (!m) continue;
  let ev;
  try {
    ev = JSON.parse(m[1]);
  } catch {
    continue;
  }
  if (typeof ev.ts === 'number') events.push(ev);
}
const newest = events.reduce((max, ev) => Math.max(max, ev.ts), 0);
const windowEnd = Number(opt('--window-end-ms', String(newest)));
const inWindow = events.filter((ev) => ev.ts > windowEnd - windowMs && ev.ts <= windowEnd);
const treesitter = inWindow.filter((ev) => ev.event === 'code_index_treesitter_failed');
const rebuilds = inWindow.filter((ev) => ev.event === 'codeinsight_rebuild');
const unattributed = treesitter.filter(
  (ev) => typeof ev.path !== 'string' || ev.path === '' || typeof ev.plugin_failure !== 'string',
);
const barePairRebuilds = rebuilds.filter((ev) => !Array.isArray(ev.changed_files) || 'stored_then_current' in ev);
const namelessRebuilds = rebuilds.filter(
  (ev) => ev.reason !== 'explicit-rebuild' && Array.isArray(ev.changed_files) && ev.changed_files.length === 0,
);

console.log(
  `window_end=${windowEnd} window_ms=${windowMs} events_in_window=${inWindow.length} treesitter_failed=${treesitter.length} ` +
    `unattributed_treesitter=${unattributed.length} rebuilds=${rebuilds.length} bare_pair_rebuilds=${barePairRebuilds.length}`,
);
const failures = [];
if (treesitter.length > 0) failures.push(`${treesitter.length} code_index_treesitter_failed in window (want 0)`);
if (unattributed.length > 0) failures.push(`${unattributed.length} treesitter failure(s) without path and plugin_failure`);
if (rebuilds.length > 1) failures.push(`${rebuilds.length} codeinsight_rebuild in window (want <=1)`);
if (barePairRebuilds.length > 0) failures.push(`${barePairRebuilds.length} rebuild(s) without changed_files (bare digest pair)`);
if (namelessRebuilds.length > 0) failures.push(`${namelessRebuilds.length} rebuild(s) naming zero changed files`);
if (failures.length === 0) {
  console.log('RESULT: PASS');
  process.exit(0);
}
console.log(`RESULT: FAIL ${failures.join('; ')}`);
process.exit(1);
