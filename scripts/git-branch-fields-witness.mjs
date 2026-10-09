// Witness for the git_branch body contract. remote:true lists the remote-tracking
// refs, all:true lists local and remote refs, a non-boolean remote or all is refused,
// and any other body field is refused with unknown_fields and accepted_fields, the
// contract git_log, git_diff and git_show already keep. Every check dispatches through
// the gm CLI to the live runtime, in scratch repositories it creates and removes.
// Usage: node scripts/git-branch-fields-witness.mjs [--gm=<gm-mcp-server.mjs>] [--project=<dir>] [--scratch=<dir>]
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const IDENTITY = ['-c', 'user.name=lanmower', '-c', 'user.email=lanmower@users.noreply.github.com'];

function parseArgs(argv) {
  const out = {};
  for (const arg of argv) {
    const m = /^--([^=]+)=(.*)$/.exec(arg);
    if (m) out[m[1]] = m[2];
  }
  return out;
}

function git(argv, cwd) {
  return execFileSync('git', [...IDENTITY, ...argv], {
    cwd,
    env: { ...process.env, GIT_TERMINAL_PROMPT: '0' },
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    timeout: 120000,
  }).trim();
}

function unquote(value) {
  return value.replace(/^'(.*)'$/, '$1');
}

function parseReply(text) {
  const reply = {};
  let list = null;
  for (const line of text.split(/\r?\n/)) {
    const item = /^  - (.*)$/.exec(line);
    if (item && list) {
      list.push(unquote(item[1]));
      continue;
    }
    const kv = /^([a-z_]+):(?: (.*))?$/.exec(line);
    if (!kv) {
      list = null;
      continue;
    }
    if (kv[2] === undefined) {
      list = [];
      reply[kv[1]] = list;
    } else {
      list = null;
      reply[kv[1]] = unquote(kv[2]);
    }
  }
  return reply;
}

function dispatch(gm, project, cwd, body) {
  const payload = JSON.stringify({ SESSION_ID: 'spoint-g-gitbranch-witness', cwd, ...body });
  const text = execFileSync(process.execPath, [gm, 'dispatch', 'git_branch', '--body', payload, '--cwd', project], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    timeout: 180000,
    maxBuffer: 16 * 1024 * 1024,
  });
  return parseReply(text);
}

const isOk = (reply) => reply.ok === 'true';
const lists = (reply, name) => (Array.isArray(reply[name]) ? reply[name] : []);
const hasAll = (reply, names) => names.every((name) => lists(reply, 'branches').includes(name));
const hasNone = (reply, names) => names.every((name) => !lists(reply, 'branches').includes(name));

function main() {
  const args = parseArgs(process.argv.slice(2));
  const gm = path.resolve(args.gm || path.join(os.homedir(), '.gm-tools', 'gm-mcp-server.mjs'));
  const project = path.resolve(args.project || process.cwd());
  const scratch = fs.mkdtempSync(path.join(args.scratch || os.tmpdir(), 'git-branch-fields-'));
  const failures = [];
  const expect = (name, ok, detail) => {
    console.log(`check=${name} ${ok ? 'pass' : 'fail'} ${detail}`);
    if (!ok) failures.push(name);
  };
  try {
    const origin = path.join(scratch, 'origin.git');
    const work = path.join(scratch, 'work');
    git(['init', '-q', '--bare', origin], scratch);
    fs.mkdirSync(work);
    git(['init', '-q', '-b', 'main', work], scratch);
    git(['commit', '-q', '--allow-empty', '-m', 'witness root'], work);
    git(['remote', 'add', 'origin', origin], work);
    git(['push', '-q', 'origin', 'main'], work);
    git(['branch', '-q', 'feature-x'], work);

    const baseline = dispatch(gm, project, work, {});
    expect(
      'baseline_local_only',
      isOk(baseline) && hasAll(baseline, ['main', 'feature-x']) && hasNone(baseline, ['origin/main', 'remotes/origin/main']),
      `ok=${baseline.ok} branches=${lists(baseline, 'branches').join('|')}`,
    );

    const remote = dispatch(gm, project, work, { remote: true });
    expect(
      'remote_lists_remote_refs',
      isOk(remote) && hasAll(remote, ['origin/main']) && hasNone(remote, ['main', 'feature-x', 'remotes/origin/main']),
      `ok=${remote.ok} branches=${lists(remote, 'branches').join('|')}`,
    );

    const all = dispatch(gm, project, work, { all: true });
    expect(
      'all_lists_local_and_remote_refs',
      isOk(all) && hasAll(all, ['main', 'feature-x', 'remotes/origin/main']),
      `ok=${all.ok} branches=${lists(all, 'branches').join('|')}`,
    );

    const both = dispatch(gm, project, work, { remote: true, all: true });
    expect(
      'remote_and_all_together_list_all',
      isOk(both) && hasAll(both, ['main', 'feature-x', 'remotes/origin/main']),
      `ok=${both.ok} branches=${lists(both, 'branches').join('|')}`,
    );

    const unknown = dispatch(gm, project, work, { bogus: 1 });
    expect(
      'unknown_field_refused_with_accepted_fields',
      !isOk(unknown) && lists(unknown, 'unknown_fields').includes('bogus') && ['remote', 'all'].every((f) => lists(unknown, 'accepted_fields').includes(f)),
      `ok=${unknown.ok} unknown_fields=${lists(unknown, 'unknown_fields').join('|')} accepted_fields=${lists(unknown, 'accepted_fields').join('|')}`,
    );

    const nonBoolean = dispatch(gm, project, work, { remote: 'origin' });
    expect(
      'non_boolean_remote_refused',
      !isOk(nonBoolean) && lists(nonBoolean, 'invalid_fields').includes('remote'),
      `ok=${nonBoolean.ok} invalid_fields=${lists(nonBoolean, 'invalid_fields').join('|')}`,
    );
  } catch (error) {
    failures.push('exception');
    console.log(`exception ${String(error.message || error).split('\n')[0]}`);
  } finally {
    fs.rmSync(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
  if (failures.length) {
    console.log(`RESULT: FAIL ${failures.join(',')}`);
    process.exitCode = 1;
  } else {
    console.log('RESULT: PASS');
  }
}

main();
