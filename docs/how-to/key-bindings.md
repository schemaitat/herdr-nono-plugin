# Add the key bindings

Herdr has no menu for plugin actions; they run from a key binding or the CLI.

## Install the bindings

```bash
herdr plugin action invoke install-keybindings --plugin nono.sandbox
```

From a checkout, `sh scripts/install-keybindings.sh` does the same. Either one
appends four `[[keys.command]]` entries to Herdr's `config.toml`, runs
`herdr config check` and `herdr server reload-config`, and skips entries that
already exist. [Key bindings](../reference/actions.md#key-bindings) lists the
chords.

## Use them alongside the Docker Sandboxes plugin

[herdr-sbx-plugin](https://github.com/dirien/herdr-sbx-plugin) uses the same
chords. Whichever installer runs second reports the taken chords instead of
replacing them. Bind the other plugin's actions to free chords yourself in
`config.toml`.

## Undo them

`herdr config reset-keys` removes every custom binding. To remove only these
four, delete their `[[keys.command]]` entries from `config.toml` and run
`herdr server reload-config`.

If `herdr config check` rejects the file after the installer edits it, the
installer restores it from a backup and the action fails with `config`.
