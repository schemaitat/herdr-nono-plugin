# Drive the plugin from scripts

## Run an action and wait for its result

`herdr plugin action invoke` returns as soon as Herdr has started the action.
To wait for the outcome, use `scripts/run-action.sh` from the plugin
directory:

```bash
sh scripts/run-action.sh info
sh scripts/run-action.sh verify-sandbox
```

It invokes the action, waits for the log entry of that very invocation (by its
`log_id`), prints the result line and the action's stderr, and exits 0 when
`ok` is true.

## Read the result

The first stdout line of every action is the result line:

```text
HERDR_SANDBOX_RESULT: {"schemaVersion":1,"plugin":"nono.sandbox","action":"start-agent","ok":true,...}
```

Strip the marker and parse the rest as JSON:

```bash
sh scripts/run-action.sh list-sandboxes | sed -n 's/^HERDR_SANDBOX_RESULT: //p' | head -1 | jq '.mappings[] | {paneId, running, verified}'
```

Branch on `ok`, then on `errorKind`. [Result line](../reference/result-line.md)
lists the fields of every action and every error kind.

## Change timeouts

Set the `HERDR_NONO_*_MS` variables in Herdr's environment, see
[Environment variables](../reference/configuration.md#environment-variables).
