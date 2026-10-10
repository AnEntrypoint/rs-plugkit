// Live witness: codeinsight outline must answer while the symbol refresh is incomplete only because it
// deferred files. A scratch project of many small files exceeds one sync pass's wall budget, so the
// first outline pass defers the tail of the listing. Requires an outline of a deferred file to answer
// with partial:true and focus state deferred, with codeinsight_index.deferred_set naming it (or, when
// deferred_set_truncated is true, naming a bounded prefix), and an outline of a covered file to answer
// with current:true. Dispatches through the installed gm CLI against the running daemon, so it reflects
// whatever plugkit build is loaded; a daemon that refuses on any deferred file prints FAIL.
// Usage: node scripts/codeinsight-outline-deferred-witness.mjs [--files 4000] [--dir <scratch>] [--timeout 300]
import { execFileSync } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : fallback;
};
const fileCount = Number(opt('--files', '4000'));
const timeoutSeconds = opt('--timeout', '300');
const dir = path.resolve(
  opt('--dir', path.join(os.tmpdir(), `codeinsight-outline-deferred-witness-${process.pid}-${Date.now()}`)),
);
const root = dir.replace(/\\/g, '/');
const cli = path.join(os.homedir(), '.gm-tools', 'gm-mcp-server.mjs');
const sessionId = 'codeinsight-outline-deferred-witness';

try {
  rmSync(dir, { recursive: true, force: true });
} catch (error) {
  console.log(`note: could not clear ${root}: ${error.code}`);
}
mkdirSync(path.join(dir, 'src'), { recursive: true });
const names = [];
for (let i = 0; i < fileCount; i += 1) {
  const name = `src/f${String(i).padStart(5, '0')}.js`;
  names.push(name);
  writeFileSync(path.join(dir, name), `export function f${i}(x) {\n  return x + ${i};\n}\n`);
}

const dispatchOutline = (target) => {
  const body = { SESSION_ID: sessionId, root, action: 'outline', path: target };
  try {
    return execFileSync(
      process.execPath,
      [cli, 'dispatch', 'codeinsight', '--body', JSON.stringify(body), '--cwd', root, '--timeout', timeoutSeconds],
      { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] },
    );
  } catch (error) {
    return `${error.stdout ?? ''}${error.stderr ?? ''}`;
  }
};

const topValue = (text, key) => {
  const m = text.match(new RegExp(`^${key}:[ \\t]*(.*)$`, 'm'));
  return m ? m[1].trim() : null;
};
const indentOf = (line) => line.length - line.trimStart().length;
const blockOf = (text, key) => {
  const lines = text.split('\n');
  const at = lines.findIndex((line) => new RegExp(`^\\s*${key}:\\s*$`).test(line));
  if (at < 0) return [];
  const keyIndent = indentOf(lines[at]);
  const body = [];
  for (const line of lines.slice(at + 1)) {
    if (line.trim() === '') continue;
    const indent = indentOf(line);
    if (indent < keyIndent || (indent === keyIndent && !line.trimStart().startsWith('-'))) break;
    body.push(line);
  }
  return body;
};
const nestedValue = (text, key) => {
  const m = text.match(new RegExp(`^\\s*${key}:[ \\t]*(\\S+)\\s*$`, 'm'));
  return m ? m[1] : null;
};

const summarize = (out) => {
  const refresh = blockOf(out, 'codeinsight_index').join('\n');
  return {
    refused: /symbol index refresh is incomplete/.test(out),
    notExecuted: /dispatch_starved_waiting_for_serial_lane|executed: false/.test(out),
    ok: topValue(out, 'ok'),
    partial: topValue(out, 'partial'),
    current: topValue(out, 'current'),
    outlineLines: blockOf(out, 'outline').filter((line) => line.trimStart().startsWith('-')).length,
    refreshComplete: nestedValue(refresh, 'complete'),
    filesDeferred: nestedValue(refresh, 'files_deferred'),
    deferredSetTruncated: nestedValue(refresh, 'deferred_set_truncated'),
    focusState: nestedValue(refresh, 'state'),
    deferredSet: blockOf(refresh, 'deferred_set')
      .map((line) => line.trim())
      .filter((line) => line.startsWith('-'))
      .map((line) => line.slice(1).trim().replace(/^['"]|['"]$/g, '')),
  };
};

const failures = [];
const notes = [];
let deferred = null;
let covered = null;

for (let attempt = 0; attempt < 3 && deferred === null && failures.length === 0; attempt += 1) {
  const target = names[names.length - 1 - attempt];
  const probe = summarize(dispatchOutline(target));
  if (probe.notExecuted || probe.refused || probe.ok !== 'true') {
    failures.push(`outline of ${target} did not answer (ok=${probe.ok} refused=${probe.refused} notExecuted=${probe.notExecuted})`);
  } else if (probe.focusState !== 'deferred') {
    notes.push(`deferred attempt ${attempt + 1}: ${target} state=${probe.focusState}`);
  } else if (probe.partial !== 'true' || probe.current !== 'false') {
    failures.push(`deferred ${target} answered without partial:true current:false (partial=${probe.partial} current=${probe.current})`);
  } else if (probe.deferredSetTruncated === 'true' ? probe.deferredSet.length === 0 : !probe.deferredSet.includes(target)) {
    failures.push(`deferred ${target} is not named by codeinsight_index.deferred_set (${probe.deferredSet.length} named, truncated=${probe.deferredSetTruncated})`);
  } else {
    deferred = {
      target,
      named: probe.deferredSet.length,
      truncated: probe.deferredSetTruncated,
      filesDeferred: probe.filesDeferred,
    };
  }
}

for (let attempt = 0; attempt < 3 && covered === null && failures.length === 0; attempt += 1) {
  const probe = summarize(dispatchOutline(names[0]));
  if (probe.notExecuted || probe.refused || probe.ok !== 'true') {
    failures.push(`outline of ${names[0]} did not answer (ok=${probe.ok} refused=${probe.refused} notExecuted=${probe.notExecuted})`);
  } else if (probe.focusState === 'covered') {
    if (probe.partial !== 'false' || probe.current !== 'true') {
      failures.push(`covered ${names[0]} answered without partial:false current:true (partial=${probe.partial} current=${probe.current})`);
    } else if (probe.outlineLines < 1) {
      failures.push(`covered ${names[0]} answered with an empty outline`);
    } else {
      covered = { target: names[0], lines: probe.outlineLines };
    }
  } else {
    notes.push(`covered attempt ${attempt + 1}: ${names[0]} state=${probe.focusState}`);
    continue;
  }
  break;
}

console.log(
  `scratch=${root} files=${fileCount} ` +
    `deferred=${deferred ? `${deferred.target} files_deferred=${deferred.filesDeferred} named=${deferred.named} truncated=${deferred.truncated}` : 'none'} ` +
    `covered=${covered ? `${covered.target} outline_lines=${covered.lines}` : 'none'}`,
);
for (const note of notes) console.log(`  note: ${note}`);
if (deferred === null && failures.length === 0) failures.push('no deferred sample: no probe landed in the deferred set');
if (covered === null && failures.length === 0) failures.push('no covered sample: the first file never answered as covered');
try {
  rmSync(dir, { recursive: true, force: true });
} catch (error) {
  console.log(`  note: scratch dir ${root} could not be removed: ${error.code}`);
}
if (failures.length === 0) {
  console.log('RESULT: PASS');
  process.exit(0);
}
console.log(`RESULT: FAIL ${failures.join('; ')}`);
process.exit(1);
