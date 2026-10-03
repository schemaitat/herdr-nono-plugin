# Verification

The bridge running in the pane verifies both sandboxes from outside, through
`/proc`, every second for up to twenty seconds after a launch
(`verifyAfterStart`), until the check passes. `verify-sandbox` and `v` in the
overlay run the same check on demand.

## Checks

| Check | Passes when |
| --- | --- |
| Confinement | Every process under either nono supervisor has `no_new_privs` set and carries `NONO_CAP_FILE` in its environment (when that is readable). |
| Server | A process in the server sandbox matches the adapter's `serverPattern` with this launch's port (OpenCode: `serve ... --port <port>`). |
| Client | The client was started with its `requiredArgs` (OpenCode: `--server http://127.0.0.1:<port>`). |
| Host service | No sandboxed process holds a TCP connection to the port of the OpenCode host service. |

## On failure

- A Herdr toast says so.
- With `onVerificationFailure: "stop"` (the default) the session is ended and
  the mapping marked `failed` with error kind `unconfined`.
- With `"warn"` the session keeps running.

Either way the report is stored on the mapping and shown by `info`,
`list-sandboxes` and the overlay.

## Output of `verify-sandbox`

```text
herdr-opencode-96db19cd2fd8: confined: 4 processes, server pid 3243022
  confined   3243039 client client ~/.opencode/bin/opencode --server http://127.0.0.1:46541
  confined   3243022 server server ~/.opencode/bin/opencode serve --hostname 127.0.0.1 --port 46541
  confined   3243410 server tool   /usr/bin/bash -c curl ... http://127.0.0.1:4096/api/info ...
  confined   3243414 server tool   curl -s -o /dev/null -w host4096=%{http_code} --max-time 3 http://127.0.0.1:4096/api/info
```

Columns: state, pid, sandbox, role (`client`, `server`, `tool`), command line.

## Escape probe

`doctor` runs a probe inside a throwaway sandbox with the server profile and
reports one line per check (`check`, `result`, `ok`, `severity`, `why`). It
fails with `unconfined` when Herdr's socket, systemd, D-Bus, Docker or the
OpenCode host service's port is reachable.
