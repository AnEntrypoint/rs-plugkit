// Witness for the gm-config prose cache that the wasm instruction verb reads.
// PASS only when the cache checkout HEAD equals the remote reference tip and the
// entry prose the cache serves contains the standing-rules marker.
// Usage: node scripts/config-cache-served-witness.mjs [--cache=<dir>] [--repo=<url>]
//        [--ref=<branch>] [--key=<prose key>] [--must=<marker>]
// Defaults: cache = <cwd>/.gm/config-source-cache-default, the implicit default tier
// that config::resolve() selects for a project without its own config source.
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const DEFAULT_REPO = 'https://github.com/AnEntrypoint/gm-config';
const DEFAULT_MUST = '## Parallel slots (standing rules)';

function parseArgs(argv) {
  const out = {};
  for (const arg of argv) {
    const m = /^--([^=]+)=(.*)$/.exec(arg);
    if (m) out[m[1]] = m[2];
  }
  return out;
}

function git(argv, cwd) {
  return execFileSync('git', argv, {
    cwd,
    env: { ...process.env, GIT_TERMINAL_PROMPT: '0' },
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    timeout: 120000,
  }).trim();
}

function firstLine(error) {
  return String(error.stderr || error.message).trim().split('\n')[0];
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const cache = path.resolve(args.cache || path.join(process.cwd(), '.gm', 'config-source-cache-default'));
  const repo = args.repo || DEFAULT_REPO;
  const ref = args.ref || 'main';
  const key = args.key || 'entry';
  const must = args.must || DEFAULT_MUST;

  let cacheHead;
  try {
    cacheHead = git(['rev-parse', 'HEAD'], cache);
  } catch (error) {
    return fail(`cache ${cache} has no readable HEAD: ${firstLine(error)}`);
  }

  let originTip;
  try {
    originTip = git(['ls-remote', '--', repo, `refs/heads/${ref}`], undefined).split(/\s+/)[0];
  } catch (error) {
    return fail(`origin ${repo} unreachable: ${firstLine(error)}`);
  }
  if (!originTip) return fail(`origin ${repo} advertises no refs/heads/${ref}`);

  const prosePath = path.join(cache, 'prose', `${key}.md`);
  let prose;
  try {
    prose = fs.readFileSync(prosePath, 'utf8').replace(/^﻿/, '').replace(/\r\n/g, '\n');
  } catch (error) {
    return fail(`served prose ${prosePath} unreadable: ${error.code || error.message}`);
  }
  const standing = prose.includes(must);

  console.log(`cache=${cache}`);
  console.log(`cache_head=${cacheHead}`);
  console.log(`origin_${ref}=${originTip}`);
  console.log(`prose=${prosePath}`);
  console.log(`standing_rules=${standing ? 'present' : 'absent'}`);

  if (cacheHead !== originTip) {
    return fail(`cache HEAD ${cacheHead} differs from origin ${ref} ${originTip}`);
  }
  if (!standing) return fail(`${key} prose lacks the marker "${must}"`);
  console.log('RESULT: PASS');
  return 0;
}

function fail(reason) {
  console.log(`RESULT: FAIL ${reason}`);
  process.exitCode = 1;
  return 1;
}

process.exitCode = main() === 0 ? 0 : 1;
