/**
 * Pane entry point: `node src/bridge.mjs <mode> --state-dir DIR --config-dir DIR --pane-id ID --plugin-root DIR`.
 * The logic lives in bridge-main.mjs so tests can import it without running it.
 */
import { runBridge } from "./bridge-main.mjs";

process.exitCode = await runBridge(process.argv.slice(2));
