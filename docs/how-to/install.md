# Install the plugin

## Check the requirements

| | |
| --- | --- |
| [Herdr](https://herdr.dev) | 0.9.0 or newer (`min_herdr_version` in the manifest) |
| [nono](https://nono.sh) | 0.78 or newer, with the OpenCode pack |
| [OpenCode](https://opencode.ai) | 2.x on the `PATH` Herdr gives plugins, signed in to a provider |
| Node.js | 20 or newer; no dependencies to install |
| Platform | Linux with Landlock (kernel 6.7+ for the network rules nono uses) |

## Install from GitHub

```bash
nono pull nolabs-ai/opencode                      # the OpenCode pack the profiles extend
herdr plugin install schemaitat/herdr-nono-plugin
```

Herdr clones the repository and runs the manifest's build step,
`scripts/write-node-path.sh`, which records where your `node` lives. Pass
`--yes` to skip the confirmation prompt.

## Check the installation

```bash
herdr plugin action list --plugin nono.sandbox          # twelve actions
herdr plugin action invoke doctor --plugin nono.sandbox
herdr plugin log list --plugin nono.sandbox --limit 1   # "ok":true in the first stdout line
```

If `doctor` fails, see [Troubleshoot](troubleshoot.md).

## Link a checkout instead

For development, link a clone. `herdr plugin link` does not run the build
step, so record the node path yourself:

```bash
git clone https://github.com/schemaitat/herdr-nono-plugin.git
cd herdr-nono-plugin && sh scripts/write-node-path.sh
herdr plugin link "$PWD"
sh scripts/run-action.sh doctor
```

`scripts/run-action.sh` runs an action and waits for its result; see
[Drive the plugin from scripts](script-actions.md).

## Next

[Add the key bindings](key-bindings.md), then start an agent with
`prefix+shift+a`.
