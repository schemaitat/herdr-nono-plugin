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
# only sandbox. The work is done by "herdr-nono run-action".
case "$0" in
  */*) here=${0%/*} ;;
  *) here=. ;;
esac
exec sh "$here/../bin/run.sh" run-action "$@"
