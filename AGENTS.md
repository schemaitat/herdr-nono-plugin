<!-- FOR AI AGENTS - Human readability is a side effect, not a goal -->
<!-- Managed by agent: keep sections and order; edit content, not structure -->
<!-- Last updated: 2026-09-29 | Last verified: 2026-09-29 -->

# AGENTS.md

**Precedence:** the **closest `AGENTS.md`** to the files you're changing wins. This is the only one in the repo.

## Project

Herdr plugin (`herdr-plugin.toml`) that runs coding agents inside nono sandboxes by shelling out to the `nono` CLI.
OpenCode runs as a private server in one nono sandbox (egress via nono's proxy) and a TUI client in another (network
blocked), joined by a port and password the bridge picks per launch.
Dependency-free Node ESM (`.mjs`, Node >= 20). No build step, no TypeScript, no bundler.

| Fact | Value |
| ------ | ------- |
| Plugin id | `nono.sandbox` (manifest `id`, `src/constants.mjs`) |
| Entry points | `src/action.mjs`, `src/bridge.mjs`, `src/events.mjs`, `src/sandboxes-pane.mjs`; each only imports its `*-main.mjs`; the manifest starts them through `bin/run.sh` (node shim) |
| External CLIs | `nono` via `HERDR_NONO_BIN`/config `nonoBin`; `herdr` via `HERDR_BIN_PATH` |
| Shipped profiles | `profiles/herdr-opencode-server.json`, `profiles/herdr-opencode-client.json` (extend `nolabs-ai/opencode`) |
| Runtime state | one JSON file per pane under `HERDR_PLUGIN_STATE_DIR/panes/` (`src/state.mjs`, version 1) |
| User config | `HERDR_PLUGIN_CONFIG_DIR/config.json` (`src/config.mjs`, unknown keys rejected) |
| Docs | `README.md` (landing page), flat `docs/` pages in the `nav` of `mkdocs.yml` (left sidebar): `getting-started`, `key-bindings`, `overlay`, `agents`, `profiles`, `troubleshooting`, `security`, `configuration`, `actions`, `design`, `development` (real-host checklist); `CHANGELOG.md`. Site: MkDocs Material (`mkdocs.yml`, `.github/mkdocs/`), published to GitHub Pages by `.github/workflows/pages.yml` |

## Commands (verified 2026-09-29)

<!-- AGENTS-GENERATED:START commands -->
| Task | Command | ~Time |
| ------ | --------- | ------- |
| Syntax check every module | `npm run syntax` | ~2s |
| Test (all) | `npm test` | ~40s |
| Test (single file) | `node --test test/actions.test.mjs` | ~15s |
| Test (name filter) | `node --test --test-name-pattern="stop" "test/*.test.mjs"` | ~10s |
| Full check (syntax + tests) | `npm run check` | ~45s |
| Docs preview / strict build | `just docs` / `just docs-build` (needs `uv`) | ~5s |
<!-- AGENTS-GENERATED:END commands -->

There is no lint, format or typecheck tool configured. `npm install` is unnecessary (zero dependencies).

## Workflow

1. **Before coding**: read this file and the Golden Samples. Real `herdr`/`nono` are not needed for the tests; they run
   the scripts against `test/fakes/nono.mjs`, `test/fakes/herdr.mjs` and `test/fakes/bin/opencode`, and the
   verification against fake `/proc` trees (`writeFakeProc` in `test/helpers.mjs`).
2. **After each change**: `node --check <file>` then the single test file that covers it.
3. **Before committing**: `npm run check`.
4. **Before claiming done**: paste the `# pass`/`# fail` summary from `npm test` as evidence. For anything touching
   the launch, the profile or the verification, also run `doctor` and `verify-sandbox` on a real host
   (`docs/development.md`, "On a real host").

## Golden Samples

| Area | File | Why |
| ------ | ------ | ----- |
| Action handler | `src/action-main.mjs` (`ACTIONS["verify-sandbox"]`, `ACTIONS.reconnect`) | Context resolution, refusal, result payload |
| Launch | `src/lifecycle.mjs` (`launch`, `verifyWhileStarting`) | Preflight, inherited terminal, verification, exit recording |
| CLI wrapper | `src/nono.mjs` | Captured runs with timeouts, output-based failure classification |
| Child-process test | `test/bridge.test.mjs` | Fixture, fake CLI logs, state assertions |
| Pure unit test | `test/verify.test.mjs` | Fake `/proc` tree |

## Heuristics (quick decisions)
<!-- AGENTS-GENERATED:START heuristics -->
| When | Do |
| ------ | ----- |
| Adding an action | Add `[[actions]]` in `herdr-plugin.toml` in the same order as `ACTIONS` (`src/action-main.mjs`), a row in both tables of `docs/actions.md`, a test in `test/actions.test.mjs`; update the action count in `docs/getting-started.md` and `docs/development.md`; `test/docs.test.mjs` enforces parity |
| Adding a config key | Add to the `PluginConfig` typedef, `CONFIG_DEFAULTS` and `validateConfig` (`src/config.mjs`), document in `docs/configuration.md`, test in `test/config.test.mjs` |
| Adding an agent | Add to `BUILTIN_AGENTS` (`src/agents.mjs`) with a profile, `requiredArgs` that keep its server inside the sandbox, and a `serverPattern` when it has a server; agents table in `docs/configuration.md`; verify on a real host first |
| Calling `nono` | Build `run` argv with `buildRunArgs` (explicit `--allow <root>`, never `--allow-cwd`, command after `--`); captured calls through `createNonoClient` (timeout); the interactive run is `spawn` with `stdio: "inherit"` in the bridge; never a shell string |
| Environment for the sandbox | Always through `sandboxEnv` (`src/lifecycle.mjs`), which drops `STRIPPED_ENV_PATTERNS` (`src/constants.mjs`) |
| Deciding a mapping is busy | `bridgeIsRunning` / `shellIsRunning` (`src/lifecycle.mjs`; pid + `processStartToken` + command line) before Herdr's agent detection |
| Judging confinement | `verifySession` (`src/verify.mjs`) from outside the sandbox; never trust output of a process inside it |
| Changing a profile | Keep `extends: "nolabs-ai/opencode"`, `linux.af_unix_mediation: "pathname"`, `environment.deny_vars` with `HERDR_*`, the server's proxy mode and the client's block; run `doctor` on a real host; update `docs/profiles.md` and `docs/security.md` |
| Reading OpenCode's service files | URL, pid and port only (`src/hostservice.mjs`); never the password |
| Choosing the workspace root | `resolveWorkspaceRoot`/`resolveWorkdir` (`src/context.mjs`) then `assertWorkspaceRoot` |
| Adding a key binding | An `add_binding` line in `scripts/install-keybindings.sh` (the chord must be absent from its `herdr_defaults` list), the key bindings tables in `README.md` and `docs/key-bindings.md` (parity test), the chord column in `docs/actions.md`, `docs/development.md` step 3 |
| Adding a manifest command | Use `["sh", "bin/run.sh", "src/<entry>.mjs"]`; `test/docs.test.mjs` rejects bare `node` |
| Calling Herdr | Go through `createHerdrClient` (`src/herdr.mjs`); check many panes with one `listPaneIds()` |
| Reporting a failure | Throw `PluginError(kind, message, {output, payload})` with a kind from `ERROR_KINDS` (`src/errors.mjs`) |
| Writing state | `savePaneEntry`/`updatePaneEntry` only (atomic, under `withPaneLock`) |
| Printing from an action | stdout is reserved for the result marker line (`emitResult`); diagnostics go to stderr |
| Printing from the bridge | stdout is the pane; use the injected `log`, and nothing while the agent's TUI runs (use a Herdr toast) |
| Adding a docs page | Prefer extending an existing page; otherwise a flat `docs/<name>.md` in `nav` in `mkdocs.yml` and the README docs list. Keep pages short, link instead of repeating; `just docs-build` must pass |
| Adding tests | `test/<area>.test.mjs`, `node:test` + `node:assert/strict`; fixtures via `test/helpers.mjs` |
| Adding a dependency | Do not |
<!-- AGENTS-GENERATED:END heuristics -->

## Boundaries

### Always Do

- Keep every exported symbol documented with a JSDoc comment.
- Keep the result marker `HERDR_SANDBOX_RESULT:` as the first stdout line of every action.
- Keep `README.md`, `herdr-plugin.toml`, `package.json` version and `CHANGELOG.md` in sync (parity test).
- Keep `--server http://127.0.0.1:{port}` a required OpenCode argument and the `serve ... --port {port}` server pattern in
  the verification.
- Use conventional commit messages (`feat:`, `fix:`, `docs:`, `test:`, `chore:`).

### Ask First

- Changing the manifest `id`, `min_herdr_version` or `platforms`.
- Changing the mapping store format (`STATE_VERSION`) or the result line schema (`RESULT_SCHEMA_VERSION`).
- Loosening the shipped profile, the stripped environment variables, or the `onVerificationFailure` default.
- Changing the network mode of either shipped profile (see `docs/profiles.md` for what breaks).

### Never Do

- Add npm dependencies or a build step.
- Launch an OpenCode client that is not pointed at the plugin's own sandboxed server (its tools would run in the
  unsandboxed host service).
- Grant `--allow-cwd`, the home directory or `/`.
- Read tokens, passwords or secrets in the plugin.
- Build shell strings from user paths without `shellQuote` (`src/shell.mjs`).
- Put raw escape bytes in source; build them with `String.fromCharCode` (see `TERMINAL_RESTORE_SEQUENCE`).

## Contracts this code depends on

| Contract | Where verified | Notes |
| ---------- | ---------------- | ------- |
| Herdr plugin env vars and `HERDR_PLUGIN_CONTEXT_JSON` fields | herdr 0.9.1 on a real host | `src/context.mjs` |
| `herdr plugin action invoke` prints `result.log.log_id` | herdr 0.9.1 | `scripts/run-action.sh` |
| `nono run --profile/--name/--allow/--read/--silent ... -- cmd`, supervisor pid = spawned pid | nono 0.78.0 | `src/nono.mjs`, `sessionForSupervisor` |
| `nono ps --json` array with `session_id`, `name`, `supervisor_pid`, `status` | nono 0.78.0 | `normalizeSessionList` |
| `nono profile show --json` resolved profile with `linux.af_unix_mediation`, `network` | nono 0.78.0 | `summarizeProfile` |
| Sandboxed processes carry `NoNewPrivs: 1` and `NONO_CAP_FILE`; the host can read their `/proc` entries | nono 0.78.0, Linux 7.0 | `src/verify.mjs` |
| `opencode serve --hostname 127.0.0.1 --port P` honours `OPENCODE_PASSWORD` (Basic auth, user `opencode`); `opencode --server URL` sends it | OpenCode 2.0.20 | `BUILTIN_AGENTS.opencode.server` |
| nono proxy mode honours `--listen-port P`; blocked mode honours `--open-port P`; proxy mode + AF_UNIX mediation rate-limits connects | nono 0.78.0 | `docs/security.md` |
| OpenCode service file `~/.local/state/opencode/service.json` with `url`, `pid` | OpenCode 2.0.18 | `src/hostservice.mjs` |

<!-- AGENTS-GENERATED:START module-boundaries -->
| Module | May import |
| -------- | ------------ |
| `constants`, `errors` | nothing from `src/` |
| `context`, `shell`, `naming`, `result`, `config`, `agents`, `procfs` | `constants`, `errors` |
| `state` | `constants`, `errors`, `context` |
| `nono`, `herdr` | `constants`, `errors` |
| `hostservice` | `procfs` |
| `verify` | `agents`, `procfs`, `hostservice` |
| `probes` | `constants`, `errors`, `hostservice` |
| `lifecycle` | everything above |
| `action-main`, `bridge-main`, `events-main`, `sandboxes-pane-main` | `lifecycle` and below; never each other |
| `action`, `bridge`, `events`, `sandboxes-pane` | only their matching logic module |
<!-- AGENTS-GENERATED:END module-boundaries -->

## When instructions conflict

Explicit user prompts override this file. Where this file and the docs disagree, fix whichever is wrong and keep
`test/docs.test.mjs` passing.
