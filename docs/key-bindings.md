# Key bindings

Herdr has no menu for plugin actions: they run from a key binding or the CLI.
These four chords are the main way to use the plugin.

| Chord | Action | Does |
| --- | --- | --- |
| `prefix+shift+a` | `start-agent` | Splits the focused pane and starts OpenCode: server sandbox in the background, client sandbox (the TUI) in the new pane |
| `prefix+shift+o` | `sandboxes` | Opens the [sandboxes overlay](overlay.md): every agent, its client and server sandbox, their processes and verification; `enter` shows an agent's details, `g` jumps to its pane, `a` cleans up all, `?` lists the keys |
| `prefix+shift+b` | `reconnect` | In an agent's pane after OpenCode exited: starts fresh sandboxes and resumes the conversation (`--continue`) |
| `prefix+shift+s` | `open-shell` | Opens a shell pane below, sandboxed with the server's profile, the one the agent's tools run under |

`prefix` is Herdr's prefix key, `ctrl+b` by default: press it, release, then
the chord.

## Install

```bash
herdr plugin action invoke install-keybindings --plugin nono.sandbox
```

From a checkout, `sh scripts/install-keybindings.sh` does the same. It appends
`[[keys.command]]` entries to Herdr's `config.toml`, runs `herdr config check`
and `herdr server reload-config`. Running it again changes nothing. If Herdr
rejects the file, it is restored from a backup and the action fails with
`config`.

## Conflicts

[herdr-sbx-plugin](https://github.com/dirien/herdr-sbx-plugin) uses the same
chords. Whichever installer runs second reports the taken chords instead of
replacing them; bind the other plugin's actions to free chords in
`config.toml` yourself. Chords Herdr binds by default are never taken.

## Other actions

The remaining actions (`stop`, `verify-sandbox`, `info`, ...) have no chord.
Most are reachable from the overlay; all run with
`herdr plugin action invoke <action> --plugin nono.sandbox`. To bind one, add
an entry like this to `config.toml`:

```toml
[[keys.command]]
key = "prefix+shift+v"
type = "plugin_action"
command = "nono.sandbox.verify-sandbox"
```

## Remove

Delete the entries from `config.toml` and run `herdr server reload-config`.
`herdr config reset-keys` removes every custom binding.
