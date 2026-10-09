import { spawn, spawnSync } from 'node:child_process'
import { createRequire } from 'node:module'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const here = path.dirname(fileURLToPath(import.meta.url))
const gmMcp = path.resolve(here, '..', '..', 'gm-mcp')
const requireFromGmMcp = createRequire(path.join(gmMcp, 'package.json'))
const yaml = requireFromGmMcp('js-yaml')
const realDispatch = path.join(gmMcp, 'src', 'dispatch.js')
const mutantMode = process.argv.includes('--mutant')
const owner = 'lane-owner'
const other = 'lane-other'

const outcomes = []
function check(name, ok, detail = '') {
  outcomes.push(Boolean(ok))
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ` :: ${detail}` : ''}`)
}

const workDir = fs.mkdtempSync(path.join(os.tmpdir(), 'gm-instruction-lease-'))
const projectRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'gm-instruction-root-'))
const holders = []

function mutantModuleUrl() {
  const source = fs.readFileSync(realDispatch, 'utf8')
  const marker = 'target.session_mismatch = identityDiffers'
  if (source.split(marker).length !== 2) throw new Error('mutant marker must occur exactly once in dispatch.js')
  const rewritten = source
    .replace(/from '\.\/([^']+)'/g, (_, rel) => `from '${pathToFileURL(path.join(gmMcp, 'src', rel)).href}'`)
    .replace(/from 'js-yaml'/g, `from '${pathToFileURL(requireFromGmMcp.resolve('js-yaml')).href}'`)
    .replace(marker, 'target.session_mismatch = true || identityDiffers')
  const file = path.join(workDir, 'dispatch.mutant.mjs')
  fs.writeFileSync(file, rewritten)
  return pathToFileURL(file).href
}

const dispatchChildFile = path.join(workDir, 'dispatch-child.mjs')
fs.writeFileSync(dispatchChildFile, `
const [moduleUrl, root, identity, sessionParam] = process.argv.slice(2)
const { gmDispatch } = await import(moduleUrl)
const body = identity ? { SESSION_ID: identity } : {}
const text = await gmDispatch({ verb: 'instruction', body, session_id: sessionParam, cwd: root, timeout_seconds: 240 })
process.stdout.write(String(text ?? ''), () => process.exit(0))
`)

const leaseHolderFile = path.join(workDir, 'lease-holder.mjs')
fs.writeFileSync(leaseHolderFile, `
const [moduleUrl, root, identity] = process.argv.slice(2)
const { ensureAgentLease } = await import(moduleUrl)
ensureAgentLease(root, identity)
process.stdout.write('ready\\n')
setInterval(() => {}, 60000)
`)

function dispatchAs(identity, sessionParam, moduleUrl) {
  const child = spawnSync(process.execPath, [dispatchChildFile, moduleUrl, projectRoot, identity ?? '', sessionParam], {
    encoding: 'utf8', windowsHide: true, timeout: 420000, maxBuffer: 64 * 1024 * 1024,
  })
  if (child.status !== 0) return { error: `dispatch child exit ${child.status}: ${String(child.stderr || '').slice(0, 400)}` }
  const payload = String(child.stdout || '').split('\n').filter(line => !line.startsWith('gm-mcp:')).join('\n')
  try {
    return yaml.load(payload) ?? { error: 'empty reply' }
  } catch (error) {
    return { error: `reply is not YAML: ${error.message}` }
  }
}

function startHolder(identity, moduleUrl) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [leaseHolderFile, moduleUrl, projectRoot, identity], {
      windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
    })
    holders.push(child)
    let out = ''
    const timer = setTimeout(() => reject(new Error('lease holder never reported ready')), 60000)
    child.stdout.on('data', chunk => {
      out += chunk
      if (out.includes('ready')) {
        clearTimeout(timer)
        resolve(child)
      }
    })
    child.on('exit', code => {
      clearTimeout(timer)
      reject(new Error(`lease holder exited with ${code} before ready`))
    })
  })
}

function stopHolder(child) {
  return new Promise(resolve => {
    if (child.exitCode !== null) return resolve()
    child.once('exit', () => resolve())
    child.kill()
  })
}

const isReply = reply => Boolean(reply && typeof reply === 'object' && !reply.error && !reply.timed_out)
const moduleUrl = mutantMode ? mutantModuleUrl() : pathToFileURL(realDispatch).href

try {
  const seed = dispatchAs(owner, 'cli-seed-owner', moduleUrl)
  check('seed: the owner dispatch returns an instruction reply', isReply(seed), seed.error ?? '')

  const liveOwner = await startHolder(owner, moduleUrl)
  const a = dispatchAs(other, 'cli-other-a', moduleUrl)
  check(
    'A: a live lease held by the owner raises session_mismatch and names that lease',
    isReply(a) && a.session_mismatch === true && a.session_owner_lease?.session_id === owner
      && a.session_owner_lease?.live === true && a.session_owner_lease?.pid === liveOwner.pid,
    `mismatch=${a.session_mismatch} lease=${JSON.stringify(a.session_owner_lease)} holder_pid=${liveOwner.pid}`,
  )
  await stopHolder(liveOwner)

  const b = dispatchAs(other, 'cli-other-b', moduleUrl)
  check(
    'B: an owner whose lease holder has died does not raise session_mismatch and reports the lease not live',
    isReply(b) && b.session_mismatch !== true && b.session_owner_lease?.session_id === owner
      && b.session_owner_lease?.live !== true,
    `mismatch=${b.session_mismatch} lease=${JSON.stringify(b.session_owner_lease)}`,
  )

  const ownerAgain = await startHolder(owner, moduleUrl)
  const c = dispatchAs(owner, 'cli-generated-c', moduleUrl)
  check(
    'C: an explicit body SESSION_ID is the identity, so a CLI-generated session id cannot shadow it',
    isReply(c) && c.session_id === owner && c.session_mismatch !== true,
    `session_id=${c.session_id} mismatch=${c.session_mismatch}`,
  )
  await stopHolder(ownerAgain)
} catch (error) {
  check('the witness ran to completion', false, error.message)
} finally {
  for (const child of holders) if (child.exitCode === null) child.kill()
  for (const dir of [workDir, projectRoot]) {
    try {
      fs.rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 300 })
    } catch (error) {
      console.log(`note: scratch directory kept (${error.code}): ${dir}`)
    }
  }
}

const passed = outcomes.length > 0 && outcomes.every(Boolean)
console.log(`RESULT: ${passed ? 'PASS' : 'FAIL'}${mutantMode ? ' (mutant: session_mismatch forced true)' : ''}`)
process.exitCode = passed ? 0 : 1
