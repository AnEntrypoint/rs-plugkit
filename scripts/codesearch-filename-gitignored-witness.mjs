// Live witness: codesearch mode "filename" with a `path` under a gitignored directory must walk that
// directory (file_source walk, walk_reason present) and return its files. Dispatches through the
// installed gm CLI against the running daemon, so it reflects whatever plugkit build is loaded.
// Usage: node scripts/codesearch-filename-gitignored-witness.mjs [--project C:/dev/spoint] [--path .gm/pool] [--query .live]
import { execFileSync } from 'node:child_process';
import os from 'node:os';
import path from 'node:path';

const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : fallback;
};
const project = opt('--project', 'C:/dev/spoint');
const scopePath = opt('--path', '.gm/pool');
const query = opt('--query', '.live');
const cli = path.join(os.homedir(), '.gm-tools', 'gm-mcp-server.mjs');
const body = {
  SESSION_ID: 'codesearch-filename-gitignored-witness',
  root: project,
  query,
  mode: 'filename',
  path: scopePath,
};

let out = '';
try {
  out = execFileSync(
    process.execPath,
    [cli, 'dispatch', 'codesearch', '--body', JSON.stringify(body), '--cwd', project],
    { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] },
  );
} catch (error) {
  out = `${error.stdout ?? ''}${error.stderr ?? ''}`;
}

const field = (name) => {
  const m = out.match(new RegExp(`^\\s*${name}:\\s*(.+)$`, 'm'));
  return m ? m[1].trim() : null;
};
const escapeRe = (s) => s.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&');
const hitRe = new RegExp(`${escapeRe(scopePath.replace(/\\/g, '/'))}/[^\\s'"]*${escapeRe(query)}`);
const hitLines = out.split('\n').filter((line) => hitRe.test(line));
const ok = field('ok');
const fileSource = field('file_source');
const matchCountRaw = field('match_count');
const hasWalkReason = /^\s*walk_reason:/m.test(out);

console.log(
  `file_source=${fileSource} match_count=${matchCountRaw} walk_reason=${hasWalkReason ? 'present' : 'absent'} hits=${hitLines.length}`,
);
const failures = [];
if (ok !== 'true') failures.push(`ok=${ok}`);
if (fileSource !== 'walk') failures.push(`file_source=${fileSource} (want walk)`);
if (!hasWalkReason) failures.push('walk_reason absent');
if (!(Number(matchCountRaw) >= 1)) failures.push(`match_count=${matchCountRaw} (want >=1)`);
if (hitLines.length === 0) failures.push(`no hit under ${scopePath} containing ${query}`);
if (failures.length === 0) {
  console.log('RESULT: PASS');
  process.exit(0);
}
console.log(`RESULT: FAIL ${failures.join('; ')}`);
process.exit(1);
