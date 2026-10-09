#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const CLI = process.env.GM_CLI || path.join(os.homedir(), '.gm-tools', 'gm-mcp-server.mjs');
const transcript = [];
const failures = [];

function check(name, ok, detail = '') {
  const line = `${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` :: ${detail}` : ''}`;
  transcript.push(line);
  console.log(line);
  if (!ok) failures.push(name);
}

const sha256 = (text) => createHash('sha256').update(text).digest('hex');

function parseJson(text) {
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}

function dispatch(root, verb, body) {
  const cli = spawnSync(
    process.execPath,
    [CLI, 'dispatch', verb, '--body', JSON.stringify(body), '--cwd', root],
    { encoding: 'utf8', windowsHide: true, timeout: 180000, maxBuffer: 64 * 1024 * 1024 },
  );
  const dispatchId = /^dispatch_id: (\S+)$/m.exec(cli.stdout ?? '')?.[1] ?? null;
  let resp = null;
  const outDir = path.join(root, '.gm', 'exec-spool', 'out');
  if (dispatchId && fs.existsSync(outDir)) {
    for (const name of fs.readdirSync(outDir)) {
      if (!name.startsWith(`${verb}-`) || !name.endsWith('.json')) continue;
      const parsed = parseJson(fs.readFileSync(path.join(outDir, name), 'utf8'));
      if (parsed?.dispatch_id === dispatchId) {
        resp = parsed;
        break;
      }
    }
  }
  let data = null;
  if (resp?.data && typeof resp.data === 'object') data = resp.data;
  else if (typeof resp?.stdout === 'string') data = parseJson(resp.stdout);
  return { dispatchId, ok: resp?.ok === true, error: resp?.error ?? null, data };
}

const idsOf = (data) => (data?.items ?? []).map((item) => item.id).join(',');
const firstRow = (data) => data?.items?.[0] ?? null;
const stamp = () => new Date().toISOString();

const root = fs.mkdtempSync(path.join(os.tmpdir(), 'gm-prd-verbs-'));
try {
  for (const id of ['a', 'b', 'c', 'd']) {
    const added = dispatch(root, 'prd-add', { id, subject: `witness row ${id}` });
    check(`prd-add ${id}`, added.ok && added.data?.added === id, added.error ?? '');
  }

  const baseline = dispatch(root, 'prd-list', {});
  check(
    'prd-list without a limit names every row',
    baseline.ok && baseline.data?.count === 4 && baseline.data?.total === 4 && baseline.data?.store_total === 4 && baseline.data?.truncated === false && baseline.data?.limit === null,
    `count=${baseline.data?.count} total=${baseline.data?.total}`,
  );

  const limited = dispatch(root, 'prd-list', { limit: 2 });
  check(
    'prd-list limit caps the items and names the total',
    limited.ok && limited.data?.count === 2 && limited.data?.total === 4 && limited.data?.truncated === true && limited.data?.limit === 2 && idsOf(limited.data) === 'a,b',
    `ids=${idsOf(limited.data)} total=${limited.data?.total}`,
  );

  const listing = dispatch(root, 'prd-list', {});
  const bindB = dispatch(root, 'prd-resolve', {
    id: 'b',
    witness_evidence: 'cross-call witness: the prd-list dispatch lists row b pending',
    witness_dispatch_id: listing.dispatchId,
  });
  check(
    'a witness_dispatch_id from an earlier CLI call verifies on a later resolve',
    bindB.ok && bindB.data?.resolved === 'b' && bindB.data?.witness_dispatch_id_verified === true,
    bindB.error ?? `verified=${bindB.data?.witness_dispatch_id_verified}`,
  );

  const pendingLimited = dispatch(root, 'prd-list', { status: 'pending', limit: 2 });
  check(
    'status filter applies before the limit',
    pendingLimited.ok && idsOf(pendingLimited.data) === 'a,c' && pendingLimited.data?.total === 3 && pendingLimited.data?.store_total === 4 && pendingLimited.data?.truncated === true,
    `ids=${idsOf(pendingLimited.data)} total=${pendingLimited.data?.total}`,
  );

  const completed = dispatch(root, 'prd-list', { status: 'completed' });
  check(
    'status completed lists only the resolved row',
    completed.ok && idsOf(completed.data) === 'b' && completed.data?.total === 1,
    `ids=${idsOf(completed.data)}`,
  );

  const zero = dispatch(root, 'prd-list', { limit: 0 });
  check(
    'limit 0 returns no items and still names the total',
    zero.ok && zero.data?.count === 0 && zero.data?.total === 4 && zero.data?.truncated === true,
    `count=${zero.data?.count} total=${zero.data?.total}`,
  );

  const badLimit = dispatch(root, 'prd-list', { limit: 'many' });
  check(
    'a non-integer limit is refused naming the limit field',
    !badLimit.ok && String(badLimit.error ?? '').includes('limit'),
    String(badLimit.error ?? 'accepted'),
  );

  const prdPath = path.join(root, '.gm', 'prd.yml');
  const longWhy = 'Row a was first written as pending and a later block closes it, so the last block is the one every listing must read. '.repeat(12).trim();
  const current = fs.readFileSync(prdPath, 'utf8');
  fs.writeFileSync(
    prdPath,
    `${current.endsWith('\n') ? current : `${current}\n`}- id: a\n  subject: witness row a\n  status: completed\n  why: "${longWhy}"\n`,
  );
  const folded = dispatch(root, 'prd-list', { id: 'a' });
  const foldedRow = firstRow(folded.data);
  check(
    'repeated blocks of one id fold to the last block and return the full why',
    folded.ok && folded.data?.count === 1 && folded.data?.total === 1 && folded.data?.store_total === 5 && foldedRow?.status === 'completed' && foldedRow?.why === longWhy,
    `count=${folded.data?.count} store_total=${folded.data?.store_total} status=${foldedRow?.status} why_len=${foldedRow?.why?.length}`,
  );

  const pendingAfterFold = dispatch(root, 'prd-list', { status: 'pending' });
  check('a folded row is judged by its last block', pendingAfterFold.ok && idsOf(pendingAfterFold.data) === 'c,d', `ids=${idsOf(pendingAfterFold.data)}`);

  const textOnly = dispatch(root, 'prd-resolve', { id: 'c', witness_evidence: 'text only witness with no dispatch id and no binding' });
  check(
    'an unbound text-only resolve is refused naming every missing field',
    !textOnly.ok && textOnly.data?.deviation_kind === 'prd-resolve-unbound-witness' && JSON.stringify(textOnly.data?.missing_fields) === JSON.stringify(['witness_dispatch_id', 'witness_exit_code', 'witness_output_sha256', 'witness_output_path', 'witness_ts']),
    textOnly.error ?? '',
  );

  const inEvidence = dispatch(root, 'prd-resolve', { id: 'c', witness_evidence: `evidence cites dispatch ${listing.dispatchId} for row c` });
  check(
    'a dispatch id that appears only in evidence text is named with its field and not verified',
    !inEvidence.ok && inEvidence.data?.witness_dispatch_id_in_evidence === listing.dispatchId && inEvidence.data?.witness_dispatch_id_field === 'witness_dispatch_id' && inEvidence.data?.witness_dispatch_id_in_ledger === true,
    inEvidence.error ?? '',
  );

  const annotated = dispatch(root, 'prd-resolve', { id: 'd', witness_evidence: 'annotation text that does not complete the row', keep_status: true });
  const dRow = firstRow(dispatch(root, 'prd-list', { id: 'd' }).data);
  check(
    'keep_status annotates without a witness and leaves the row pending',
    annotated.ok && annotated.data?.annotated === 'd' && annotated.data?.status_kept === true && dRow?.status === 'pending',
    annotated.error ?? `status=${dRow?.status}`,
  );

  const bogus = dispatch(root, 'prd-resolve', { id: 'c', witness_evidence: 'bogus dispatch probe for row c', witness_dispatch_id: '1-1-abc' });
  check(
    'a dispatch id this project never recorded is refused as not_recorded',
    !bogus.ok && bogus.data?.deviation_kind === 'prd-resolve-fabricated-dispatch' && bogus.data?.reason === 'not_recorded',
    bogus.error ?? '',
  );

  const noWitness = dispatch(root, 'prd-resolve', { id: 'c' });
  check('a resolve with no witness at all is refused', !noWitness.ok && noWitness.data?.deviation_kind === 'prd-resolve-no-witness', noWitness.error ?? '');

  const witnessOut = path.join(root, '.gm', 'witness-out');
  fs.mkdirSync(witnessOut, { recursive: true });
  const phaseAText = `${transcript.join('\n')}\n`;
  fs.writeFileSync(path.join(witnessOut, 'phase-a.txt'), phaseAText);
  const phaseAExit = failures.length === 0 ? 0 : 1;
  const bindingBase = {
    witness_exit_code: 0,
    witness_output_sha256: sha256(phaseAText),
    witness_output_path: '.gm/witness-out/phase-a.txt',
    witness_ts: stamp(),
  };

  const boundC = dispatch(root, 'prd-resolve', {
    id: 'c',
    witness_evidence: 'phase A transcript of the prd verbs witness binds row c',
    ...bindingBase,
    witness_exit_code: phaseAExit,
  });
  check(
    'exit code, output hash, path and timestamp bind a witness and close row c',
    boundC.ok && boundC.data?.resolved === 'c' && boundC.data?.witness_bound === true && boundC.data?.witness_binding?.output_sha256 === sha256(phaseAText) && boundC.data?.witness_binding?.exit_code === phaseAExit,
    boundC.error ?? '',
  );
  const storedC = firstRow(dispatch(root, 'prd-list', { id: 'c' }).data);
  check(
    'the bound row stores its witness_binding',
    storedC?.status === 'completed' && storedC?.witness_binding?.output_path === '.gm/witness-out/phase-a.txt' && storedC?.witness_binding?.output_sha256 === sha256(phaseAText),
    `status=${storedC?.status}`,
  );

  const flipped = sha256(phaseAText).replace(/.$/, (ch) => (ch === '0' ? '1' : '0'));
  const negatives = [
    ['a non-zero exit code', { ...bindingBase, witness_exit_code: 1 }, 'witness_exit_code'],
    ['an output hash that does not match the file', { ...bindingBase, witness_output_sha256: flipped }, 'witness_output_sha256'],
    ['a malformed timestamp', { ...bindingBase, witness_ts: '2026-13-45T99:00:00Z' }, 'witness_ts'],
    ['a missing output file', { ...bindingBase, witness_output_path: '.gm/witness-out/missing.txt' }, 'witness_output_path'],
    ['an output path that escapes the project root', { ...bindingBase, witness_output_path: '../escape.txt' }, 'witness_output_path'],
  ];
  for (const [label, fields, field] of negatives) {
    const refused = dispatch(root, 'prd-resolve', { id: 'd', witness_evidence: `negative probe with ${label}`, ...fields });
    check(`binding refused for ${label}`, !refused.ok && refused.data?.field === field, refused.error ?? '');
  }

  const partial = dispatch(root, 'prd-resolve', { id: 'd', witness_evidence: 'negative probe with a partial binding', witness_exit_code: 0 });
  check(
    'a partial binding is refused naming the missing fields',
    !partial.ok && JSON.stringify(partial.data?.missing_fields) === JSON.stringify(['witness_output_sha256', 'witness_output_path', 'witness_ts']),
    partial.error ?? '',
  );

  const duplicate = dispatch(root, 'prd-resolve', { id: 'd', witness_evidence: 'duplicate probe reusing the phase A output', ...bindingBase });
  check(
    'the same witness output cannot close a second row',
    !duplicate.ok && duplicate.data?.deviation_kind === 'prd-resolve-duplicate-witness' && duplicate.data?.duplicate_of === 'c',
    duplicate.error ?? '',
  );
  const stillPending = firstRow(dispatch(root, 'prd-list', { id: 'd' }).data);
  check('refused bindings leave the row pending', stillPending?.status === 'pending', `status=${stillPending?.status}`);

  const phaseBText = `${transcript.join('\n')}\n`;
  fs.writeFileSync(path.join(witnessOut, 'phase-b.txt'), phaseBText);
  const boundD = dispatch(root, 'prd-resolve', {
    id: 'd',
    witness_evidence: 'phase B transcript of the prd verbs witness binds row d',
    witness_exit_code: failures.length === 0 ? 0 : 1,
    witness_output_sha256: sha256(phaseBText),
    witness_output_path: '.gm/witness-out/phase-b.txt',
    witness_ts: stamp(),
  });
  check('a second, distinct output closes a second row', boundD.ok && boundD.data?.resolved === 'd' && boundD.data?.witness_bound === true, boundD.error ?? '');

  const rebound = dispatch(root, 'prd-resolve', {
    id: 'c',
    witness_evidence: 'phase A transcript of the prd verbs witness binds row c',
    ...bindingBase,
    witness_exit_code: phaseAExit,
  });
  check('re-resolving a row with its own binding is idempotent', rebound.ok && rebound.data?.resolved === 'c', rebound.error ?? '');
} finally {
  try {
    fs.rmSync(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 250 });
  } catch (error) {
    console.log(`NOTE scratch project not removed (${error.code ?? error.message}): ${root}`);
  }
}

console.log(failures.length === 0 ? 'RESULT: PASS' : `RESULT: FAIL (${failures.length} checks failed: ${failures.join('; ')})`);
process.exitCode = failures.length === 0 ? 0 : 1;
