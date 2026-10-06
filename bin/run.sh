#!/bin/sh
# Runs the plugin binary: bin/run.sh <subcommand> [arguments]. Herdr starts the
# plugin's commands through this shim, which finds bin/herdr-nono next to it
# (installed by scripts/install-binary.sh) or at HERDR_NONO_BINARY, and execs
# it. When the binary is missing it says so, in the result format an action's
# caller parses and on stderr, instead of failing with a bare "not found".
# Only shell builtins are used, so a PATH without coreutils cannot break it.
case "$0" in
  */*) dir=${0%/*} ;;
  *) dir=. ;;
esac
binary="${HERDR_NONO_BINARY:-$dir/herdr-nono}"
if [ ! -x "$binary" ]; then
  if [ -n "${HERDR_PLUGIN_ACTION_ID:-}" ]; then
    printf 'HERDR_SANDBOX_RESULT: {"schemaVersion":1,"plugin":"nono.sandbox","action":"%s","ok":false,"errorKind":"startup","message":"the plugin binary %s is missing; run sh scripts/install-binary.sh in the plugin directory"}\n' "$HERDR_PLUGIN_ACTION_ID" "$binary"
  fi
  printf 'herdr-nono-plugin: the plugin binary %s is missing. Run "sh scripts/install-binary.sh" in the plugin directory (it downloads the release or builds from source), or set HERDR_NONO_BINARY.\n' "$binary" >&2
  # In a Herdr pane the pane closes with this process; give the message time to be read.
  if [ -t 1 ] && command -v sleep >/dev/null 2>&1; then sleep 8; fi
  exit 127
fi
exec "$binary" "$@"
