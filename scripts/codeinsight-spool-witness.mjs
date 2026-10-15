import { readFileSync } from 'node:fs';

const TREESITTER_FAILED = 'code_index_treesitter_failed';
const REBUILD = 'codeinsight_rebuild';
const CHECKED_EVENTS = [TREESITTER_FAILED, REBUILD];
const DEFAULT_LOG_PATH = 'C:/dev/spoint/.gm/exec-spool/.watcher.log';
const DEFAULT_WINDOW_MS = '600000';

const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : fallback;
};
const logPath = opt('--log', DEFAULT_LOG_PATH);
const windowMs = Number(opt('--window-ms', DEFAULT_WINDOW_MS));

const text = readFileSync(logPath, 'utf8');
const parsed = [];
for (const line of text.split('\n')) {
  const m = line.match(/evt:\s*(\{.*\})\s*$/);
  if (!m) continue;
  try {
    parsed.push(JSON.parse(m[1]));
  } catch {
    continue;
  }
}
const events = parsed.filter((ev) => typeof ev.ts === 'number');
const windowEnd = Number(opt('--window-end-ms', String(Date.now())));
const inWindow = events.filter((ev) => ev.ts > windowEnd - windowMs && ev.ts <= windowEnd);
const treesitter = inWindow.filter((ev) => ev.event === TREESITTER_FAILED);
const rebuilds = inWindow.filter((ev) => ev.event === REBUILD);
const unattributed = treesitter.filter(
  (ev) => typeof ev.path !== 'string' || ev.path === '' || typeof ev.plugin_failure !== 'string',
);
const barePairRebuilds = rebuilds.filter((ev) => !Array.isArray(ev.changed_files) || 'stored_then_current' in ev);
const namelessRebuilds = rebuilds.filter(
  (ev) => ev.reason !== 'explicit-rebuild' && Array.isArray(ev.changed_files) && ev.changed_files.length === 0,
);
const unparsed = Object.fromEntries(
  CHECKED_EVENTS.map((name) => [
    name,
    Math.max(0, text.split(name).length - 1 - parsed.filter((ev) => ev.event === name).length),
  ]),
);
const unparsedTotal = Object.values(unparsed).reduce((sum, n) => sum + n, 0);

console.log(
  `window_end=${windowEnd} window_ms=${windowMs} events_in_window=${inWindow.length} treesitter_failed=${treesitter.length} ` +
    `unattributed_treesitter=${unattributed.length} rebuilds=${rebuilds.length} bare_pair_rebuilds=${barePairRebuilds.length} ` +
    `unparsed_checked=${unparsedTotal}`,
);
const failures = [];
if (inWindow.length === 0) failures.push('no spool events in window (log not live or watcher silent)');
if (treesitter.length > 0) failures.push(`${treesitter.length} code_index_treesitter_failed in window (want 0)`);
if (unattributed.length > 0) failures.push(`${unattributed.length} treesitter failure(s) without path and plugin_failure`);
if (rebuilds.length > 1) failures.push(`${rebuilds.length} codeinsight_rebuild in window (want <=1)`);
if (barePairRebuilds.length > 0) failures.push(`${barePairRebuilds.length} rebuild(s) without changed_files (bare digest pair)`);
if (namelessRebuilds.length > 0) failures.push(`${namelessRebuilds.length} rebuild(s) naming zero changed files`);
if (unparsedTotal > 0) failures.push(`checked record(s) not parseable as JSON: ${JSON.stringify(unparsed)}`);
if (failures.length === 0) {
  console.log('RESULT: PASS');
  process.exit(0);
}
console.log(`RESULT: FAIL ${failures.join('; ')}`);
process.exit(1);
