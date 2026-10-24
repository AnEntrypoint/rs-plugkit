import { execFileSync, spawn, spawnSync } from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { WASI } from 'node:wasi';

const DEFAULT_REPO = 'https://github.com/AnEntrypoint/gm-config';
const DEFAULT_MUST = '## Parallel slots';
const DEFAULT_PROBE_ROOTS = '8';
const GLOBAL_PROBE_DIR = 'config-source-remote-probe';
const STALE_HEAD = 'a'.repeat(40);
const CARGO_BUILD_TIMEOUT_MS = 900000;

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

function fail(reason) {
  console.log(`RESULT: FAIL ${reason}`);
  process.exitCode = 1;
  return 1;
}

const SHIM_SOURCE = `use std::process::Command;
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = std::env::var("GIT_SHIM_LOG").unwrap_or_default();
    if !dir.is_empty() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name = format!("{}/{}-{}.argv", dir, std::process::id(), nanos);
        let _ = std::fs::write(name, args.join(" "));
    }
    if args.iter().any(|a| a == "clone" || a == "fetch") {
        eprintln!("git {} refused by the remote-probe witness", args.first().map(|s| s.as_str()).unwrap_or("?"));
        std::process::exit(1);
    }
    let real = std::env::var("GIT_SHIM_REAL").unwrap_or_else(|_| "git".to_string());
    let status = Command::new(real).args(&args).status();
    std::process::exit(status.map(|s| s.code().unwrap_or(1)).unwrap_or(1));
}
`;

const NATIVE_ENTRY_HARNESS_SOURCE = `fn main() {
    match rs_plugkit::config_sync_native::ensure_default_cache() {
        Ok((cache, degraded)) => println!("ok cache={} degraded={}", cache, degraded.unwrap_or_else(|| "none".to_string())),
        Err(reason) => println!("err {}", reason),
    }
}
`;

function repoRoot() {
  return path.resolve(path.join(path.dirname(fileURLToPath(import.meta.url)), '..'));
}

function run(cmd, argv, cwd, timeoutMs) {
  const r = execFileSync(cmd, argv, {
    cwd,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    timeout: timeoutMs,
    windowsHide: true,
  });
  return r;
}

function realGitPath() {
  const raw =
    process.platform === 'win32'
      ? run('where', ['git'], undefined, 30000)
      : run('sh', ['-c', 'command -v git'], undefined, 30000);
  const first = raw.split('\n').map((l) => l.trim()).filter(Boolean)[0];
  if (!first) throw new Error('no git on PATH for the shim to forward to');
  return first;
}

function ensureNativeRlib(root) {
  run('cargo', ['build', '--features', 'slim'], root, CARGO_BUILD_TIMEOUT_MS);
  const rlib = path.join(root, 'target', 'debug', 'librs_plugkit.rlib');
  if (!fs.existsSync(rlib)) throw new Error(`no native rlib at ${rlib} after cargo build`);
  return rlib;
}

function buildHarness(root, tmp, rlib) {
  const src = path.join(tmp, 'harness.rs');
  fs.writeFileSync(src, NATIVE_ENTRY_HARNESS_SOURCE);
  const bin = path.join(tmp, process.platform === 'win32' ? 'harness.exe' : 'harness');
  run(
    'rustc',
    ['--edition', '2021', src, '-o', bin, '--extern', `rs_plugkit=${rlib}`, '-L', `dependency=${path.join(root, 'target', 'debug', 'deps')}`],
    tmp,
    600000,
  );
  return bin;
}

function buildShim(tmp, realGit) {
  const src = path.join(tmp, 'shim.rs');
  fs.writeFileSync(src, SHIM_SOURCE);
  const binDir = path.join(tmp, 'bin');
  fs.mkdirSync(binDir, { recursive: true });
  const bin = path.join(binDir, process.platform === 'win32' ? 'git.exe' : 'git');
  run('rustc', ['--edition', '2021', '-O', src, '-o', bin], tmp, 600000);
  if (process.platform !== 'win32') fs.chmodSync(bin, 0o755);
  return { binDir, realGit };
}

function seedRoot(tmp, index, head) {
  const root = path.join(tmp, 'roots', `r${index}`);
  const cache = path.join(root, '.gm', 'config-source-cache-default');
  const gitDir = path.join(cache, '.git');
  fs.mkdirSync(path.join(gitDir, 'objects'), { recursive: true });
  fs.mkdirSync(path.join(gitDir, 'refs', 'heads'), { recursive: true });
  fs.writeFileSync(path.join(gitDir, 'HEAD'), `${head}\n`);
  return root;
}

function spawnHarness(bin, cwd, env) {
  return new Promise((resolve) => {
    const child = spawn(bin, [], { cwd, env, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
    let out = '';
    child.stdout.on('data', (d) => (out += d));
    child.stderr.on('data', (d) => (out += d));
    child.on('error', (e) => resolve({ code: -1, out: String(e.message) }));
    child.on('close', (code) => resolve({ code, out: out.trim() }));
  });
}

function countProbes(logDir) {
  let n = 0;
  for (const name of fs.readdirSync(logDir)) {
    const text = fs.readFileSync(path.join(logDir, name), 'utf8');
    if (/^ls-remote\b/.test(text.trim())) n += 1;
  }
  return n;
}


async function probeSection(root, originTip, roots) {
  const nativeSrc = fs.readFileSync(
    path.join(root, 'crates', 'plugkit-core', 'src', 'config_sync_native.rs'),
    'utf8',
  );
  const links = [
    ['no magic debounce constant remains', () => !/const DEBOUNCE_MS/.test(nativeSrc)],
    ['freshness is a derived decision', () => /fn probe_decision\(/.test(nativeSrc) && /enum ProbeDecision \{\s*Due,/.test(nativeSrc)],
    ['a probe runs only when the machine cannot already answer it', () => /rec\.remote_sha\.is_empty\(\)/.test(nativeSrc) && /rec\.advertised_since_ms/.test(nativeSrc)],
    ['a project behind the recorded tip materializes without probing', () => /if have\.as_deref\(\) == Some\(remote\.as_str\(\)\)/.test(nativeSrc) && /fn materialize\(/.test(nativeSrc)],
    ['the measured change interval replaces the constant', () => /rec\.change_interval_ms/.test(nativeSrc) && /UNMEASURED_REF_INTERVAL_MS/.test(nativeSrc)],
    ['the record is machine-global', () => /crate::config::global_probe_root\(/.test(nativeSrc)],
    ['the record key is (repo, reference)', () => /format!\("\{repo\}\\u\{0\}\{reference\}"\)/.test(nativeSrc)],
    ['the probe is single-flight and its answer is shared', () => /fn wait_for_shared_probe\(/.test(nativeSrc)],
    ['the record is kept in memory as well as on disk', () => /static REMEMBERED_REMOTE/.test(nativeSrc) && /fn remember_remote\(/.test(nativeSrc)],
  ];
  let missing = 0;
  for (const [name, holds] of links) {
    const pass = holds();
    console.log(`${pass ? 'ok     ' : 'MISSING'} ${name}`);
    if (!pass) missing += 1;
  }
  if (missing > 0) return `remote probe: ${missing} missing link(s) in config_sync_native.rs`;

  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'config-remote-probe-'));
  try {
    const rlib = ensureNativeRlib(root);
    const harness = buildHarness(root, tmp, rlib);
    const { binDir, realGit } = buildShim(tmp, realGitPath());
    const logDir = path.join(tmp, 'gitlog');
    fs.mkdirSync(logDir, { recursive: true });
    const home = path.join(tmp, 'home');
    fs.mkdirSync(home, { recursive: true });
    const env = {
      ...process.env,
      HOME: home,
      USERPROFILE: home,
      PATH: [binDir, process.env.PATH].filter(Boolean).join(path.delimiter),
      GIT_SHIM_LOG: logDir,
      GIT_SHIM_REAL: realGit,
      GIT_TERMINAL_PROMPT: '0',
    };

    const behind = [];
    for (let i = 0; i < roots; i += 1) behind.push(seedRoot(tmp, i, STALE_HEAD));
    const runs = await Promise.all(behind.map((cwd) => spawnHarness(harness, cwd, env)));
    const probes = countProbes(logDir);
    const recordDir = path.join(home, '.gm', GLOBAL_PROBE_DIR);
    const record = firstRecord(recordDir);
    console.log(`probe_roots=${roots}`);
    console.log(`ls_remote_calls=${probes}`);
    console.log(`record_probe_count=${record ? record.probe_count : 'absent'}`);
    console.log(`record_remote_sha=${record ? record.remote_sha : 'absent'}`);
    if (runs.some((r) => r.code !== 0)) return `a harness process exited nonzero: ${runs.map((r) => r.code).join(',')}`;
    if (probes !== 1) return `${roots} project roots behind the recorded tip cost ${probes} ls-remote, expected exactly 1`;
    if (!record) return 'no machine-global remote-tip record was written';
    if (record.probe_count !== 1) return `the record counts ${record.probe_count} probes, expected 1`;
    if (record.remote_sha !== originTip) return `the record holds ${record.remote_sha}, expected ${originTip}`;

    const settled = seedRoot(tmp, roots, originTip);
    const settledRun = await spawnHarness(harness, settled, env);
    const afterProbes = countProbes(logDir);
    const afterRecord = firstRecord(recordDir);
    console.log(`settled_root_ls_remote_calls=${afterProbes - probes}`);
    console.log(`settled_root_out=${settledRun.out}`);
    if (settledRun.code !== 0) return `a root already at the recorded tip exited ${settledRun.code}`;
    if (afterProbes !== probes) return `a root at the recorded tip triggered ${afterProbes - probes} more ls-remote, expected 0`;
    if (afterRecord.probe_count !== 1) return `a root at the recorded tip advanced the probe count to ${afterRecord.probe_count}`;
    console.log(`RESULT: PASS ${probes} ls-remote for ${roots} project roots`);
    return null;
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

function firstRecord(dir) {
  if (!fs.existsSync(dir)) return null;
  for (const name of fs.readdirSync(dir)) {
    if (name.endsWith('.json')) return JSON.parse(fs.readFileSync(path.join(dir, name), 'utf8'));
  }
  return null;
}

function gitArgsFromHostPayload(raw) {
  try {
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed : String(parsed).split(/\s+/).filter(Boolean);
  } catch {
    return raw.split(/\s+/).filter(Boolean);
  }
}

function answerRootProbe(args, rootPosix) {
  if (args.includes('--show-toplevel')) {
    return { ok: true, exit_code: 0, stdout: `${rootPosix}\n`, stderr: '' };
  }
  if (args.includes('--is-inside-work-tree')) {
    return { ok: true, exit_code: 0, stdout: 'true\n', stderr: '' };
  }
  return null;
}

async function instantiateWasm(module, root, home, onGit, note) {
  const kv = new Map();
  const rootAbs = path.resolve(root);
  const homeAbs = path.resolve(home);
  const rootPosix = rootAbs.replace(/\\/g, '/');
  const homePosix = homeAbs.replace(/\\/g, '/');
  const allowed = (p) => {
    const abs = path.resolve(p);
    return abs === rootAbs || abs.startsWith(rootAbs + path.sep) || abs === homeAbs || abs.startsWith(homeAbs + path.sep);
  };
  const statOrNull = (file) => (allowed(file) && fs.existsSync(file) ? fs.statSync(file) : null);
  let exp = null;
  const str = (ptr, len) => Buffer.from(exp.memory.buffer, ptr, len).toString('utf8');
  const alloc = (bytes) => {
    const ptr = exp.plugkit_alloc(bytes.length);
    new Uint8Array(exp.memory.buffer, ptr, bytes.length).set(bytes);
    return ptr;
  };
  const pack = (text) => {
    const bytes = Buffer.from(text, 'utf8');
    if (bytes.length === 0) return 0n;
    return BigInt(alloc(bytes)) | (BigInt(bytes.length) << 32n);
  };
  const guarded = (fn, failure) => (...args) => {
    try {
      return fn(...args);
    } catch {
      return failure;
    }
  };
  const hostEnv = {
    host_now_ms: () => BigInt(Date.now()),
    host_cwd: () => pack(rootPosix),
    host_log: () => 0,
    host_fs_allow_root: () => 1,
    host_fs_read: guarded((p, pl) => {
      const file = path.resolve(str(p, pl));
      const st = statOrNull(file);
      if (!st || st.isDirectory()) return 0n;
      const text = fs.readFileSync(file, 'utf8');
      return text.length === 0 ? 1n : pack(text);
    }, 0n),
    host_fs_write: guarded((p, pl, dp, dl) => {
      const file = path.resolve(str(p, pl));
      note(`write ${allowed(file) ? '' : 'DENIED '}${file}`);
      if (!allowed(file)) return 0;
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, str(dp, dl), 'utf8');
      return 1;
    }, 0),
    host_fs_cas_write: guarded((p, pl, ep, el, dp, dl) => {
      const file = path.resolve(str(p, pl));
      if (!allowed(file)) return 0;
      const current = statOrNull(file) ? fs.readFileSync(file, 'utf8') : '';
      if (current !== str(ep, el)) return 2;
      fs.mkdirSync(path.dirname(file), { recursive: true });
      fs.writeFileSync(file, str(dp, dl), 'utf8');
      return 1;
    }, 0),
    host_fs_remove: guarded((p, pl) => {
      const file = path.resolve(str(p, pl));
      const st = statOrNull(file);
      if (!st || st.isDirectory()) return 0;
      fs.unlinkSync(file);
      return 1;
    }, 0),
    host_fs_readdir: guarded((p, pl) => {
      const dir = path.resolve(str(p, pl));
      const st = statOrNull(dir);
      if (!st || !st.isDirectory()) return 0n;
      const entries = fs.readdirSync(dir, { withFileTypes: true }).map((e) => ({
        name: e.name,
        is_file: e.isFile(),
        is_dir: e.isDirectory(),
      }));
      return pack(JSON.stringify(entries));
    }, 0n),
    host_fs_stat: guarded((p, pl) => {
      const st = statOrNull(path.resolve(str(p, pl)));
      if (!st) return 0n;
      return pack(JSON.stringify({ isFile: st.isFile(), isDirectory: st.isDirectory(), size: st.size, mtime_ms: st.mtimeMs }));
    }, 0n),
    host_kv_get: (np, nl, kp, kl) => {
      const value = kv.get(`${str(np, nl)}\u0000${str(kp, kl)}`);
      return value === undefined ? 0n : pack(value);
    },
    host_kv_put: (np, nl, kp, kl, vp, vl) => {
      kv.set(`${str(np, nl)}\u0000${str(kp, kl)}`, str(vp, vl));
      return 1;
    },
    host_kv_delete: (np, nl, kp, kl) => {
      kv.delete(`${str(np, nl)}\u0000${str(kp, kl)}`);
      return 1;
    },
    host_kv_query: () => 0n,
    host_vec_embed: () => -1,
    host_vec_search: () => 0n,
    host_exec_js: guarded((cp, cl) => {
      const run = spawnSync(process.execPath, ['-e', str(cp, cl)], { encoding: 'utf8', timeout: 30000, windowsHide: true });
      return pack(JSON.stringify({ stdout: run.stdout ?? '', stderr: run.stderr ?? '', exit_code: run.status, timed_out: run.error?.code === 'ETIMEDOUT' }));
    }, 0n),
    host_fetch: () => 0n,
    host_env_get: guarded((kp, kl) => {
      const key = str(kp, kl);
      return key === 'HOME' || key === 'USERPROFILE' ? pack(homePosix) : 0n;
    }, 0n),
    host_random_fill: (ptr, len) => {
      crypto.randomFillSync(new Uint8Array(exp.memory.buffer, ptr, len));
      return 1;
    },
    host_task_proc: () => 0n,
    host_plugin_call: () => 0n,
    browser_cdp: () => 0n,
    host_git: (ap, al, cp, cl) => {
      try {
        const args = gitArgsFromHostPayload(str(ap, al));
        const cwd = cl > 0 ? str(cp, cl) : undefined;
        return pack(JSON.stringify(onGit(args, cwd, rootPosix)));
      } catch (e) {
        note(`host_git threw: ${e instanceof Error ? e.message : String(e)}`);
        return 0n;
      }
    },
  };
  const wasi = new WASI({ version: 'preview1', args: [], env: {}, preopens: {} });
  const wasiImports = typeof wasi.getImportObject === 'function' ? wasi.getImportObject() : { wasi_snapshot_preview1: wasi.wasiImport };
  const env = { ...hostEnv };
  for (const imp of WebAssembly.Module.imports(module)) {
    if (imp.module === 'env' && !(imp.name in env)) env[imp.name] = () => 0;
  }
  const instance = await WebAssembly.instantiate(module, { env, ...wasiImports });
  exp = instance.exports;
  wasi.initialize(instance);
  const call = (verb, body) => {
    const verbBytes = Buffer.from(verb, 'utf8');
    const bodyBytes = Buffer.from(body, 'utf8');
    const vp = alloc(verbBytes);
    const bp = alloc(bodyBytes);
    const packed = exp.dispatch_verb(vp, verbBytes.length, bp, bodyBytes.length);
    exp.plugkit_free(vp, verbBytes.length);
    exp.plugkit_free(bp, bodyBytes.length);
    const ptr = Number(packed & 0xffffffffn);
    const len = Number(packed >> 32n);
    if (ptr === 0 || len === 0) return '';
    const text = Buffer.from(exp.memory.buffer, ptr, len).toString('utf8');
    exp.plugkit_free(ptr, len);
    return text;
  };
  return { call };
}

function ensureWasm(root) {
  run('cargo', ['+nightly', 'build', '--target', 'wasm32-wasip3', '--features', 'slim,wit-env'], root, CARGO_BUILD_TIMEOUT_MS);
  const wasm = path.join(root, 'target', 'wasm32-wasip3', 'debug', 'rs_plugkit.wasm');
  if (!fs.existsSync(wasm)) throw new Error(`no wasm at ${wasm} after cargo build`);
  return wasm;
}

async function wasmSection(root, originTip, roots) {
  const syncSrc = fs.readFileSync(path.join(root, 'crates', 'plugkit-core', 'src', 'config_sync.rs'), 'utf8');
  const links = [
    ['wasm twin resolves the tip from the machine-global record', () => /crate::config::global_probe_root\(/.test(syncSrc) && /fn remote_probe_paths\(/.test(syncSrc)],
    ['wasm twin derives freshness from the pin and the recorded sha', () => /fn probe_decision\(/.test(syncSrc) && /rec\.change_interval_ms/.test(syncSrc)],
    ['wasm twin keeps the record in memory as well as on disk', () => /static REMEMBERED_REMOTE/.test(syncSrc) && /fn remember_remote\(/.test(syncSrc)],
    ['wasm twin probes single-flight through a host lock', () => /try_lock_at\(&paths\.lock\(\), &paths\.root\)/.test(syncSrc)],
    ['wasm twin asks the global record instead of probing per dispatch', () => /global_remote_tip\(src, now\)/.test(syncSrc) && /fn probe_and_record\(/.test(syncSrc)],
  ];
  let missing = 0;
  for (const [name, holds] of links) {
    const pass = holds();
    console.log(`${pass ? 'ok     ' : 'MISSING'} ${name}`);
    if (!pass) missing += 1;
  }
  if (missing > 0) return `remote probe (wasm): ${missing} missing link(s) in config_sync.rs`;

  const wasmPath = ensureWasm(root);
  const module = await WebAssembly.compile(fs.readFileSync(wasmPath));
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), 'config-wasm-probe-'));
  try {
    const home = path.join(tmp, 'home');
    fs.mkdirSync(home, { recursive: true });
    const probes = [];
    const gitCalls = [];
    const onGit = (args, cwd, rootPosix) => {
      gitCalls.push(`${args[0]}${cwd ? ` @${cwd}` : ''}`);
      const rootProbe = answerRootProbe(args, rootPosix);
      if (rootProbe) return rootProbe;
      if (args[0] === 'ls-remote') probes.push(args.join(' '));
      if (args[0] === 'clone' || args[0] === 'fetch') {
        return { ok: false, exit_code: 1, stdout: '', stderr: `git ${args[0]} refused by the remote-probe witness` };
      }
      const r = spawnSync('git', args, { cwd, encoding: 'utf8', timeout: 120000, windowsHide: true, env: { ...process.env, GIT_TERMINAL_PROMPT: '0' } });
      return { ok: r.status === 0, exit_code: r.status ?? 1, stdout: r.stdout ?? '', stderr: r.stderr ?? '' };
    };
    const rooted = [];
    for (let i = 0; i < roots; i += 1) rooted.push(seedRoot(tmp, 100 + i, STALE_HEAD));
    const instances = [];
    const note = (s) => gitCalls.push(s);
    for (const dir of rooted) instances.push(await instantiateWasm(module, dir, home, onGit, note));
    const replies = instances.map((inst, i) => inst.call('config_resolve', JSON.stringify({ session_id: `wasm-probe-${i}` })));
    const afterRoots = probes.length;
    const record = firstRecord(path.join(home, '.gm', GLOBAL_PROBE_DIR));
    console.log(`wasm_probe_roots=${roots}`);
    console.log(`wasm_git_calls=${gitCalls.length}: ${gitCalls.slice(0, 12).join(' | ')}`);
    console.log(`wasm_first_reply=${(replies[0] || '').slice(0, 400)}`);
    console.log(`wasm_ls_remote_calls=${afterRoots}`);
    console.log(`wasm_record_probe_count=${record ? record.probe_count : 'absent'}`);
    console.log(`wasm_record_remote_sha=${record ? record.remote_sha : 'absent'}`);
    if (replies.some((r) => r.trim() === '')) return 'a wasm config_resolve returned no reply';
    if (afterRoots !== 1) return `${roots} wasm project roots behind the recorded tip cost ${afterRoots} ls-remote, expected exactly 1`;
    if (!record) return 'the wasm twin wrote no machine-global remote-tip record';
    if (record.probe_count !== 1) return `the wasm record counts ${record.probe_count} probes, expected 1`;
    if (record.remote_sha !== originTip) return `the wasm record holds ${record.remote_sha}, expected ${originTip}`;

    const settled = seedRoot(tmp, 100 + roots, originTip);
    const settledInstance = await instantiateWasm(module, settled, home, onGit, note);
    const settledReply = settledInstance.call('config_resolve', JSON.stringify({ session_id: 'wasm-probe-settled' }));
    const afterSettled = probes.length;
    const afterRecord = firstRecord(path.join(home, '.gm', GLOBAL_PROBE_DIR));
    console.log(`wasm_settled_root_ls_remote_calls=${afterSettled - afterRoots}`);
    if (settledReply.trim() === '') return 'the settled wasm root returned no reply';
    if (afterSettled !== afterRoots) return `a settled wasm root triggered ${afterSettled - afterRoots} more ls-remote, expected 0`;
    if (afterRecord.probe_count !== 1) return `a settled wasm root advanced the probe count to ${afterRecord.probe_count}`;
    console.log(`RESULT: PASS ${afterRoots} ls-remote for ${roots} wasm project roots`);
    return null;
  } catch (error) {
    return error instanceof Error ? error.message : String(error);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const cache = path.resolve(args.cache || path.join(process.cwd(), '.gm', 'config-source-cache-default'));
  const repo = args.repo || DEFAULT_REPO;
  const ref = args.ref || 'main';
  const key = args.key || 'entry';
  const must = args.must || DEFAULT_MUST;
  const roots = Number.parseInt(args['probe-roots'] ?? DEFAULT_PROBE_ROOTS, 10);

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

  const probeFailure = await probeSection(repoRoot(), originTip, roots);
  if (probeFailure) return fail(probeFailure);

  const wasmFailure = await wasmSection(repoRoot(), originTip, roots);
  if (wasmFailure) return fail(wasmFailure);

  console.log('RESULT: PASS');
  return 0;
}

process.exitCode = (await main()) === 0 ? 0 : 1;
