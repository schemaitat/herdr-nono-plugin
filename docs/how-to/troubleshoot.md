# Troubleshoot

Start with `doctor`; most problems show up there. `doctor` also prints the
plugin's state and config directories, which the steps below refer to.

## `doctor` fails

- **With `startup`:** `nono` is not on the `PATH` Herdr started with. Set
  `nonoBin` in `config.json`.
- **With `config`, mentioning `nolabs-ai/opencode`:** run
  `nono pull nolabs-ai/opencode`.
- **With `unconfined` and `probe FAIL` lines:** the configured profile lets
  the probe reach a host socket. Use the shipped profile, or add
  `"linux": { "af_unix_mediation": "pathname" }` to yours.
- **With `unconfined`, mentioning the OpenCode background service:** your
  server profile lets the tools reach localhost. Use the shipped one, or run
  `opencode service stop`.

## The agent does not start

- **The pane says `nono: the agent was NOT started`:** as above, your server
  profile reaches localhost while the OpenCode host service runs. Use the
  shipped profile or run `opencode service stop`, then `reconnect`
  (`prefix+shift+b`).
- **The pane says `OpenCode's server exited before it answered` or `did not
  answer`:** read the log it names, `logs/<session>-server.log` in the state
  directory.
- **The agent exits at once:** run `open-shell` and start it by hand to see
  the error. A launch stopped by the verification says so in the pane and in
  `info` (`lastError`).
- **An action fails with "node was not found":** run
  `sh scripts/write-node-path.sh` in the plugin directory, or set
  `HERDR_NONO_NODE` to your node binary in Herdr's environment.

## A tool fails inside the sandbox

- **"could not connect to proxy", or a download stalls:** nono 0.78
  rate-limits connection checks in proxy mode, see
  [the explanation](../explanation/security.md#nono-rate-limits-connections-in-proxy-mode).
  Retry, or space requests by 0.1 s.
- **`git push` or `git clone` over SSH fails:** only HTTPS through the proxy
  reaches the internet. Use HTTPS remotes.
- **OpenCode cannot copy to the clipboard:** the X11 and Wayland sockets are
  Unix sockets, and the profile blocks them on purpose.

## Harmless messages

- **`IPC denial: ... connect /var/run/nscd/socket`:** glibc looked for the
  name service cache daemon, which is not installed.

## Reproduce a launch by hand

Run the command the pane runs, from the plugin directory:

```bash
node src/bridge.mjs start --state-dir <state dir> --config-dir <config dir> --pane-id <pane id> --plugin-root "$PWD"
```
