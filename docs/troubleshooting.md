# Troubleshooting

Start with `doctor`. It also prints the plugin's state and config directories.

| Symptom | Fix |
| --- | --- |
| `doctor` fails with `startup` | `nono` is not on the `PATH` Herdr started with. Set `nonoBin` in `config.json`. |
| `doctor` fails with `config`, mentioning `nolabs-ai/opencode` | `nono pull nolabs-ai/opencode` |
| `doctor` fails with `unconfined` and `probe FAIL` lines | Your profile reaches a host socket. Use the shipped profile or add `"linux": { "af_unix_mediation": "pathname" }`. See [Profiles](profiles.md). |
| `nono: the agent was NOT started`, or `doctor` mentions the OpenCode background service | Your server profile reaches localhost while the host service runs. Use the shipped profile, or `opencode service stop`, then `prefix+shift+b`. |
| `OpenCode's server exited before it answered` / `did not answer` | Read `logs/<session>-server.log` in the state directory. |
| The agent exits at once | `prefix+shift+s` and start `opencode` by hand to see the error. A launch stopped by verification says so in the pane and in `info` (`lastError`). |
| An action fails with "the plugin binary ... is missing" | The install step did not finish (no network and no `cargo`). Run `sh scripts/install-binary.sh` in the plugin directory, or set `HERDR_NONO_BINARY` to a binary you built. |
| "could not connect to proxy", or downloads stall | nono's rate limit in proxy mode ([Security](security.md#known-limitations)). Retry, or space requests out. |
| `git` over SSH fails | Only HTTPS leaves the sandbox. Use HTTPS remotes. |
| No clipboard in OpenCode | X11 and Wayland sockets are blocked on purpose. |
| `IPC denial: ... /var/run/nscd/socket` | Harmless; glibc looked for a daemon that is not installed. |

To reproduce a launch by hand, run what the pane runs, from the plugin
directory:

```bash
bin/herdr-nono bridge start --state-dir <state dir> --config-dir <config dir> --pane-id <pane id> --plugin-root "$PWD"
```
