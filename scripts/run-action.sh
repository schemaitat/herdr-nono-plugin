#!/bin/sh
# Runs one plugin action and waits for its result.
# Usage: scripts/run-action.sh <action> [seconds]
#
# "herdr plugin action invoke" returns as soon as Herdr has started the action;
# the outcome lands in the plugin log a moment later. This waits for the log
# entry of this very invocation (matched by the log_id invoke prints, so an
# older run of the same action is never mistaken for it; default 90 seconds),
# prints the HERDR_SANDBOX_RESULT line and the action's stderr, and exits 0
# when the result says ok, 1 when it does not, 2 on misuse or timeout.
# HERDR_BIN_PATH names the herdr binary; otherwise herdr on PATH. Runs from any
# host terminal: pane actions use the pane focused in Herdr, or the workspace's
# only sandbox.
set -u
case "$0" in
  */*) here=${0%/*} ;;
  *) here=. ;;
esac
action=${1:-}
limit=${2:-90}
if [ -z "$action" ]; then
  printf 'usage: %s <action> [seconds]\n' "$0" >&2
  exit 2
fi
herdr=${HERDR_BIN_PATH:-herdr}
if ! invoked=$("$herdr" plugin action invoke "$action" --plugin nono.sandbox); then
  printf 'could not start %s; is the plugin installed and enabled?\n' "$action" >&2
  exit 2
fi
log_id=$(printf '%s' "$invoked" | sh "$here/../bin/run.sh" -e 'let text = ""; process.stdin.on("data", (chunk) => { text += chunk; }).on("end", () => { try { process.stdout.write(JSON.parse(text)?.result?.log?.log_id ?? ""); } catch { /* not JSON */ } });')
# Prints the finished entry for this invocation, exit 3 while it is still running.
parse='
const { action, logId } = process.env;
const logs = JSON.parse(require("fs").readFileSync(0, "utf8"))?.result?.logs ?? [];
const entry = logs.find((item) => (logId ? item.log_id === logId : item.action_id === action));
if (!entry || entry.status === "running") process.exit(3);
const first = (entry.stdout ?? "").split("\n")[0];
if (entry.stderr) process.stderr.write(entry.stderr.trim() + "\n");
process.stdout.write(first + "\n");
process.exit(entry.status === "succeeded" && first.includes("\"ok\":true") ? 0 : 1);
'
elapsed=0
while [ "$elapsed" -lt "$limit" ]; do
  sleep 1
  elapsed=$((elapsed + 1))
  "$herdr" plugin log list --plugin nono.sandbox --limit 20 | action="$action" logId="$log_id" sh "$here/../bin/run.sh" -e "$parse"
  code=$?
  if [ "$code" -ne 3 ]; then
    exit "$code"
  fi
  if [ "$elapsed" -eq 5 ]; then
    printf 'still running; stop waits for the agent to exit, doctor runs an escape probe\n' >&2
  fi
done
printf '%s did not finish within %s seconds\n' "$action" "$limit" >&2
exit 2
