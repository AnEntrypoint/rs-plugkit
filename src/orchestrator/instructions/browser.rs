pub const TEXT: &str = r#"# BROWSER

YOU drive the browser through the spool. Plugkit holds the Chromium handle, the per-project profile, the session table; you advance the work by writing `.gm/exec-spool/in/browser/<N>.txt` and reading `out/<N>.json`. There is no library import that shortcuts this. There is no puppeteer/playwright/CDP handle you can hold. The verb is the surface; everything else is fabrication.

The body is a string. Four shapes, nothing else:

```
session new
session list
session kill <id>
<arbitrary JS expression evaluated in page context>
```

A bare expression with no live session opens one and evaluates against `about:blank`. A bare expression with a live session reuses it. `session new` returns the id you carry on subsequent dispatches; you keep it in your turn and refer to it by writing `session=<id>\n<expr>` when more than one is open.

## Envelope

You read `{ok, stdout, stderr, exit_code, session_id?}`. `stdout` is the stringified evaluation result. `stderr` carries page errors and launch diagnostics. `exit_code` non-zero = the dispatch you fired did not land; you read `stderr` and re-dispatch, you do not retry blind.

## Headed by default

The window opens on the user's screen. That is the witness — you launched, they saw the tab, the DOM mutated visibly. `GM_BROWSER_HEADLESS=1` opts into headless; absent that env, a session with no visible window is a launch you did not actually make. Do not assume headless. Do not request headless to "be quiet". The flash IS the proof.

## Profile

Per-project at `<cwd>/.gm/browser-profile/`. Cookies, storage, extensions persist across your session, across your turns, across runs. A second concurrent launch contends the SingletonLock; plugkit falls back to a per-pid profile and the fallback loses persistence — you avoid concurrent launches by reusing the session id you already hold.

## Discipline

You never spawn Chromium yourself. You never `npm i puppeteer`. You never shell `chrome.exe`. The verb owns the handle; bypassing it orphans state plugkit cannot reap and breaks the next session's first read. When the page needs navigation, you evaluate `location.href = '...'` through the spool. When it needs a screenshot, you dispatch the verb that returns one — you do not reach for a library to take it.

A dispatch that returns `ok:false` with a launch error is plugkit telling you the environment refused; you read the `stderr`, you dispatch `instruction`, you do not loop the same body waiting for a different answer.
"#;
