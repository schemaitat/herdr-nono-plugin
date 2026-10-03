# Run another agent

The plugin ships one adapter, OpenCode. Other agents can run as custom agents
in a single sandbox. Custom agents are a starting point, not a tested
configuration: check the result with `verify-sandbox` and `open-shell` before
you rely on it.

## Add the agent

In `config.json` (`herdr plugin config-dir nono.sandbox`), describe the agent
under `customAgents` and select it with `agentKind`. For Claude Code with
nono's Claude pack:

```json
{
  "agentKind": "claude-code",
  "customAgents": {
    "claude-code": {
      "title": "Claude Code",
      "command": ["claude"],
      "defaultArgs": [],
      "resumeArgs": ["--continue"],
      "profile": "nolabs-ai/claude",
      "herdrDetectionKind": "claude"
    }
  }
}
```

Pull the profile first (`nono pull nolabs-ai/claude`), then press
`prefix+shift+a`.

## Agents Herdr does not recognise

Set `herdrDetectionKind` to `null`. The plugin then reports the pane to Herdr
as an agent while it runs (`reportAgentStatus`).

## Agents that start their own server

If the agent runs tools in a server process, set `serverPattern` to a regular
expression matching that server's command line. The verification then fails
unless a process inside the sandbox matches it, so a client talking to a
server outside the sandbox is caught.

## Arguments that must never change

List them in `requiredArgs`. They always come right after `command`, and
`agentArgs` cannot remove them.

[Configuration](../reference/configuration.md#custom-agent-fields) lists every
field.
