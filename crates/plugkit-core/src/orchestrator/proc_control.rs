use serde_json::{json, Value};

const PROC_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_LIMIT: u64 = 100;
const MAX_LIMIT: u64 = 1_000;
const SIGNALS: [&str; 4] = ["TERM", "INT", "HUP", "KILL"];
const RESULT_PREFIX: &str = "GM_PROC_RESULT ";

const PROC_BODY: &str = r###"
const cp = await import('node:child_process');
const selfPid = process.pid;
const listing = process.platform === 'win32'
  ? cp.execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', "Get-CimInstance Win32_Process | ForEach-Object { $_.ProcessId.ToString() + ' ' + $_.ParentProcessId.ToString() + ' ' + $_.CommandLine }"], { encoding: 'utf8', maxBuffer: 268435456 })
  : cp.execFileSync('ps', ['-A', '-o', 'pid=', '-o', 'ppid=', '-o', 'command='], { encoding: 'utf8', maxBuffer: 268435456 });
const rows = [];
for (const line of listing.split(/\r?\n/)) {
  const m = line.match(/^\s*(\d+)\s+(\d+)\s?(.*)$/);
  if (m) rows.push({ pid: Number(m[1]), ppid: Number(m[2]), args: m[3] });
}
const byPid = new Map(rows.map((r) => [r.pid, r]));
const excluded = new Set([selfPid]);
for (const r of rows) if (r.ppid === selfPid) excluded.add(r.pid);
let cur = selfPid;
for (let hops = 0; hops < 64 && byPid.has(cur); hops += 1) {
  const parent = byPid.get(cur).ppid;
  if (parent <= 0) break;
  excluded.add(parent);
  cur = parent;
}
const out = (value) => console.log('GM_PROC_RESULT ' + JSON.stringify(value));
const visible = rows.filter((r) => !excluded.has(r.pid) && (req.pattern === null || r.args.includes(req.pattern)));
if (req.op === 'list') {
  out({ ok: true, count: visible.length, truncated: visible.length > req.limit, processes: visible.slice(0, req.limit).map((r) => ({ pid: r.pid, ppid: r.ppid, args: r.args.slice(0, 400) })) });
} else if (req.pid !== null && excluded.has(req.pid)) {
  out({ ok: false, error: 'refusing to signal this process chain', error_code: 'self_excluded', pid: req.pid });
} else {
  const targets = req.pid !== null ? rows.filter((r) => r.pid === req.pid) : visible;
  if (targets.length === 0) {
    out({ ok: false, error: 'no matching process', error_code: 'not_found' });
  } else if (targets.length > 1 && !req.all) {
    out({ ok: false, error: 'pattern matches ' + targets.length + ' processes; pass all:true or a pid', error_code: 'ambiguous', pids: targets.map((r) => r.pid) });
  } else {
    const signalled = targets.map((r) => {
      try {
        process.kill(r.pid, 'SIG' + req.signal);
        return { pid: r.pid, signal: req.signal, sent: true };
      } catch (error) {
        return { pid: r.pid, signal: req.signal, sent: false, error: String(error.code || error.message) };
      }
    });
    out({ ok: signalled.every((r) => r.sent), signalled });
  }
}
"###;

fn fail(error: &str, error_code: &str) -> Value {
    json!({ "ok": false, "error": error, "error_code": error_code })
}

fn request_from(content: &str, op: &str) -> Result<Value, Value> {
    let body: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    let pattern = match body.get("pattern") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        Some(_) => return Err(fail("pattern must be a non-empty string", "invalid_args")),
    };
    let pid = match body.get("pid") {
        None | Some(Value::Null) => None,
        Some(value) => match value.as_u64() {
            Some(pid) if pid > 1 => Some(pid),
            _ => return Err(fail("pid must be an integer above 1", "invalid_args")),
        },
    };
    let signal = match body.get("signal") {
        None | Some(Value::Null) => "TERM".to_string(),
        Some(Value::String(name)) => {
            let upper = name.trim().to_ascii_uppercase();
            let bare = upper.strip_prefix("SIG").unwrap_or(upper.as_str()).to_string();
            if !SIGNALS.contains(&bare.as_str()) {
                return Err(fail("signal must be one of TERM, INT, HUP, KILL", "invalid_args"));
            }
            bare
        }
        Some(_) => return Err(fail("signal must be a string", "invalid_args")),
    };
    let all = body.get("all").and_then(Value::as_bool).unwrap_or(false);
    let limit = match body.get("limit") {
        None | Some(Value::Null) => DEFAULT_LIMIT,
        Some(value) => match value.as_u64() {
            Some(limit) if (1..=MAX_LIMIT).contains(&limit) => limit,
            _ => return Err(fail("limit must be an integer 1..=1000", "invalid_args")),
        },
    };
    if op == "signal" && pattern.is_some() == pid.is_some() {
        return Err(fail("signal needs exactly one of pid or pattern", "invalid_args"));
    }
    Ok(json!({
        "op": op,
        "pattern": pattern,
        "pid": pid,
        "signal": signal,
        "all": all,
        "limit": limit,
    }))
}

fn request_code(request: &Value) -> String {
    format!("const req = {};\n{}", request, PROC_BODY)
}

#[cfg(target_arch = "wasm32")]
fn run_host(code: &str) -> Value {
    let opts = json!({ "timeoutMs": PROC_TIMEOUT_MS }).to_string();
    let packed = unsafe {
        crate::wasm_dispatch::host_exec_js(
            code.as_ptr(),
            code.len() as u32,
            opts.as_ptr(),
            opts.len() as u32,
        )
    };
    match crate::wasm_dispatch::unpack_to_string_pub(packed) {
        Some(raw) => serde_json::from_str(&raw).unwrap_or(Value::String(raw)),
        None => json!({ "ok": false, "error_code": "process_host_empty", "error": "process host returned empty" }),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn run_host(code: &str) -> Value {
    let _ = (code, PROC_TIMEOUT_MS);
    json!({ "ok": false, "error_code": "process_control_unavailable_on_native", "error": "process control is unavailable on native target" })
}

fn finish(host: &Value) -> (String, String, i32) {
    let parsed = host
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or("")
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix(RESULT_PREFIX))
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    match parsed {
        Some(result) => {
            let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(false);
            (result.to_string(), String::new(), if ok { 0 } else { 1 })
        }
        None => (
            json!({
                "ok": false,
                "error": host
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("process host produced no result"),
                "error_code": "failed",
                "exit_code": host.get("exit_code").cloned().unwrap_or(Value::Null),
                "stderr": host.get("stderr").cloned().unwrap_or(Value::Null),
            })
            .to_string(),
            String::new(),
            1,
        ),
    }
}

pub fn handle_list(content: &str) -> (String, String, i32) {
    match request_from(content, "list") {
        Ok(request) => finish(&run_host(&request_code(&request))),
        Err(error) => (error.to_string(), String::new(), 1),
    }
}

pub fn handle_signal(content: &str) -> (String, String, i32) {
    match request_from(content, "signal") {
        Ok(request) => finish(&run_host(&request_code(&request))),
        Err(error) => (error.to_string(), String::new(), 1),
    }
}
