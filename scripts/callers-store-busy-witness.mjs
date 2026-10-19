import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { randomBytes } from 'node:crypto';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const WITNESS_ROOT = process.env.GM_WITNESS_ROOT || resolve(fileURLToPath(new URL('..', import.meta.url)));
const DEFAULT_ROOT = WITNESS_ROOT;
const DEFAULT_SPOOL_ROOT = join(WITNESS_ROOT, '.gm', 'exec-spool');
const DEFAULT_BUDGET_MS = '90000';
const DEFAULT_SESSION = 'callers-store-busy-witness';
const SPOOL_IDLE_WAIT_MS = 120000;
const HOLDER_LIFETIME_SLACK_MS = 30000;
const REPLY_GRACE_MS = 15000;
const RAW_LOCK_ERROR = /database is locked|\.lock\b/i;

const argv = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] !== undefined ? argv[i + 1] : fallback;
};
const root = resolve(opt('--root', DEFAULT_ROOT));
const spoolRoot = resolve(opt('--spool', DEFAULT_SPOOL_ROOT));
const budgetMs = Number(opt('--budget-ms', DEFAULT_BUDGET_MS));
const session = opt('--session', DEFAULT_SESSION);
const evaluatePath = opt('--evaluate', '');

function replyBody(reply) {
  return reply && reply.data && typeof reply.data === 'object' ? reply.data : reply || {};
}

function evaluateReply(reply, elapsedMs, budget) {
  const body = replyBody(reply);
  const refresh = body.codeinsight_index || {};
  const failures = [];
  if (RAW_LOCK_ERROR.test(JSON.stringify(reply || {}))) failures.push('raw store lock error reached the reply');
  const busy = body.store === 'busy' || refresh.store_busy === true;
  if (!busy) {
    failures.push(
      `no busy handling (store=${JSON.stringify(body.store)} refresh.store_busy=${JSON.stringify(refresh.store_busy)} refresh.complete=${JSON.stringify(refresh.complete)})`,
    );
  }
  if (refresh.complete === true) failures.push('refresh completed while the lock was held, so the lock never reached the write path');
  if (!Array.isArray(body.edges)) failures.push('reply carries no edges array');
  if (typeof body.answered_from !== 'string' || body.answered_from === '') failures.push('reply does not name its answer source');
  if (elapsedMs > budget) failures.push(`reply took ${elapsedMs} ms, over the ${budget} ms budget`);
  return failures;
}

function finish(failures, detail = '') {
  if (detail) console.log(detail);
  if (failures.length === 0) {
    console.log('RESULT: PASS');
    process.exitCode = 0;
    return;
  }
  console.log(`RESULT: FAIL ${failures.join('; ')}`);
  process.exitCode = 1;
}

function sleep(ms) {
  return new Promise((done) => setTimeout(done, ms));
}

async function waitFor(probe, timeoutMs, pollMs) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = probe();
    if (value) return value;
    if (Date.now() >= deadline) return null;
    await sleep(pollMs);
  }
}

function holderMain() {
  const fs = require('node:fs');
  const lock = process.argv[1];
  const deadlineMs = Number(process.argv[2]);
  let created = false;
  process.on('exit', () => {
    if (created) fs.rmSync(lock, { recursive: true, force: true });
  });
  process.on('SIGINT', () => process.exit(130));
  process.on('SIGTERM', () => process.exit(143));
  try {
    fs.mkdirSync(lock);
    created = true;
  } catch (error) {
    process.stdout.write('REFUSED ' + error.code + '\n');
    process.exit(3);
  }
  process.stdout.write('HELD ' + lock + '\n');
  process.stdin.setEncoding('utf8');
  process.stdin.on('data', (chunk) => {
    if (chunk.includes('release')) process.exit(0);
  });
  process.stdin.on('end', () => process.exit(0));
  setTimeout(() => process.exit(0), deadlineMs);
}

const dispatchWaitPath = join(spoolRoot, '.dispatch-wait.json');
const WAITING_STATES = new Set(['claimed_waiting_for_admission', 'claimed_waiting_for_serial_lane', 'queued']);

function spoolIdle() {
  try {
    const ledger = JSON.parse(readFileSync(dispatchWaitPath, 'utf8'));
    return !(ledger.requests || []).some((request) => WAITING_STATES.has(request.state));
  } catch (error) {
    return error.code === 'ENOENT';
  }
}

const holderSource = `(${holderMain.toString()})();`;
let releaseHolderLock = async () => {};

async function run() {
  if (evaluatePath) {
    const reply = JSON.parse(readFileSync(resolve(evaluatePath), 'utf8'));
    finish(evaluateReply(reply, Number(opt('--elapsed-ms', '0')), budgetMs), `evaluate ${resolve(evaluatePath)}`);
    return;
  }
  const lockDir = join(root, '.gm', 'gm.db.lock');
  if (!existsSync(root)) return finish([`project root missing: ${root}`]);
  const idleBy = Date.now() + SPOOL_IDLE_WAIT_MS;
  while (!spoolIdle() && Date.now() < idleBy) await sleep(500);
  if (!spoolIdle()) return finish([`spool dispatches stayed queued for 120 s; NOT RUN, no lock taken`]);
  if (existsSync(lockDir)) {
    return finish([`store lock already present at ${lockDir}; another writer holds it, so this witness neither takes nor removes it`]);
  }

  const holder = spawn(process.execPath, ['-e', holderSource, lockDir, String(budgetMs + HOLDER_LIFETIME_SLACK_MS)], {
    stdio: ['pipe', 'pipe', 'inherit'],
  });
  holder.stdin.on('error', () => {});
  holder.stdout.setEncoding('utf8');
  let holderOut = '';
  let holderExited = false;
  holder.stdout.on('data', (chunk) => {
    holderOut += chunk;
  });
  holder.on('exit', () => {
    holderExited = true;
  });
  releaseHolderLock = async () => {
    if (!holderExited) {
      holder.stdin.end('release\n');
      await waitFor(() => holderExited, 5000, 50);
      if (!holderExited) holder.kill();
      await waitFor(() => holderExited, 2000, 50);
    }
    if (holderOut.includes('HELD') && existsSync(lockDir)) rmSync(lockDir, { recursive: true, force: true });
  };

  const heldState = await waitFor(
    () => (holderOut.includes('HELD') ? 'held' : holderExited ? 'refused' : null),
    10000,
    50,
  );
  if (heldState !== 'held') {
    await releaseHolderLock();
    return finish([`holder did not take the lock: ${holderOut.trim() || 'holder exited before taking it'}`]);
  }

  const task = `${session}-${process.pid}-${Date.now()}-${randomBytes(4).toString('hex')}`;
  const inDir = join(spoolRoot, 'in', 'callers');
  const requestPath = join(inDir, `${task}.txt`);
  mkdirSync(inDir, { recursive: true });
  writeFileSync(
    `${requestPath}.tmp`,
    JSON.stringify({ symbol: 'callers', root: root.replace(/\\/g, '/'), SESSION_ID: session }),
    'utf8',
  );
  const startedAt = Date.now();
  renameSync(`${requestPath}.tmp`, requestPath);
  const outPath = join(spoolRoot, 'out', `callers-${task}.json`);
  const reply = await waitFor(
    () => {
      if (!existsSync(outPath)) return null;
      try {
        return JSON.parse(readFileSync(outPath, 'utf8'));
      } catch {
        return null;
      }
    },
    budgetMs + REPLY_GRACE_MS,
    250,
  );
  const elapsedMs = Date.now() - startedAt;
  await releaseHolderLock();
  if (!reply) return finish([`no callers reply within ${budgetMs + REPLY_GRACE_MS} ms`], `task=${task}`);

  const body = replyBody(reply);
  const refresh = body.codeinsight_index || {};
  console.log(
    `task=${task} elapsed_ms=${elapsedMs} store=${JSON.stringify(body.store ?? null)} ` +
      `refresh_store_busy=${JSON.stringify(refresh.store_busy ?? null)} ` +
      `edges=${Array.isArray(body.edges) ? body.edges.length : 'none'} ` +
      `answered_from=${JSON.stringify(body.answered_from ?? null)} error_code=${JSON.stringify(reply.error_code ?? null)}`,
  );
  return finish(evaluateReply(reply, elapsedMs, budgetMs));
}

process.on('SIGINT', async () => {
  await releaseHolderLock();
  process.exit(130);
});

run().catch(async (error) => {
  await releaseHolderLock();
  finish([`witness error: ${error && error.message ? error.message : error}`]);
});
