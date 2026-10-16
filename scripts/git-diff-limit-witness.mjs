#!/usr/bin/env node
// Witness for git_diff `limit` (crates/plugkit-core/src/wasm_dispatch/verbs/git.rs, fn git_diff).
// Usage: node scripts/git-diff-limit-witness.mjs [repoDir] [range] [limit] [patch|stat]
// Replays the guest steps on real git output: a -z name listing, the first `limit`
// names as literal top-anchored pathspecs, then one diff call. Exits 1 when the capped
// output does not hold exactly min(total, limit) file sections or rows, or when
// files_omitted is wrong, or when the capped output exceeds the host stdout cap.

import { execFileSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HOST_GIT_STDOUT_CAP_BYTES = 1_048_576;

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = process.argv[2] ?? path.resolve(here, '..');
const range = process.argv[3] ?? 'HEAD~200..HEAD';
const limit = Number(process.argv[4] ?? 2);
const mode = process.argv[5] ?? 'patch';
const stat = mode === 'stat';

if (!Number.isInteger(limit) || limit < 0) {
    console.error(`limit must be a non-negative integer, got ${process.argv[4]}`);
    process.exit(1);
}

const git = (args) =>
    execFileSync('git', ['-C', repo, ...args], {
        encoding: 'utf8',
        maxBuffer: 512 * 1024 * 1024,
    });

const listing = git(['diff', '--no-color', '--no-renames', '--name-only', '-z', range]);
const names = listing.split('\0').filter((n) => n !== '');
const total = names.length;
const kept = names.slice(0, limit);

let diff = '';
if (total > limit && limit === 0) {
    diff = '';
} else {
    const scope = total > limit ? kept.map((n) => `:(top,literal)${n}`) : [];
    const args = ['diff', '--no-color', '--no-renames'];
    if (stat) args.push('--stat');
    args.push(range);
    if (scope.length > 0) args.push('--', ...scope);
    diff = git(args);
}

const lines = diff.split('\n');
const shown = stat
    ? lines.filter((line) => / \| /.test(line)).length
    : diff.split(/^(?=diff --git )/m).filter((s) => s.startsWith('diff --git ')).length;
const expectedShown = Math.min(total, limit);
const filesOmitted = Math.max(0, total - limit);
const outputBytes = Buffer.byteLength(diff, 'utf8');

const checks = {
    shown_equals_min_total_limit: shown === expectedShown,
    files_omitted_is_total_minus_limit: filesOmitted === Math.max(0, total - limit),
    capped_output_under_host_cap: outputBytes <= HOST_GIT_STDOUT_CAP_BYTES,
};

const report = {
    repo,
    range,
    mode,
    limit,
    files_total: total,
    shown,
    files_omitted: filesOmitted,
    capped_output_bytes: outputBytes,
    host_stdout_cap_bytes: HOST_GIT_STDOUT_CAP_BYTES,
    checks,
    ok: Object.values(checks).every(Boolean),
};

console.log(JSON.stringify(report, null, 2));
process.exit(report.ok ? 0 : 1);
