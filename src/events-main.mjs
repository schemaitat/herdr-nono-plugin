/**
 * Event hook for `worktree.removed`: stops the sandboxed agents that were
 * started for the removed worktree and forgets their mappings, unless the
 * user set `cleanupOnWorktreeRemoved` to false. Nothing is deleted: a nono
 * sandbox keeps no state of its own, and the agent's conversation stays in
 * the agent's own storage.
 * @module events-main
 */
import { loadConfig } from "./config.mjs";
import { readEventPayload, readPluginEnv } from "./context.mjs";
import { errorMessageOf } from "./errors.mjs";
import { createHerdrClient } from "./herdr.mjs";
import { bridgeIsRunning, createLifecycle, shellIsRunning } from "./lifecycle.mjs";
import { createNonoClient } from "./nono.mjs";
import { deletePaneEntry, entriesForLocalPath, getPaneEntry, loadState, withPaneLock } from "./state.mjs";

/**
 * Extracts the removed worktree path from the event payload.
 * @param {Record<string, any>} payload
 * @returns {string|null}
 */
export function removedWorktreePath(payload) {
  return payload?.data?.worktree?.path ?? payload?.worktree?.path ?? null;
}

async function handleEventUnsafe(env) {
  const pluginEnv = readPluginEnv(env);
  const log = (line) => process.stdout.write(`${line}\n`);
  if (pluginEnv.eventName !== "worktree.removed") {
    log(`ignoring event ${pluginEnv.eventName ?? "(none)"}`);
    return 0;
  }
  if (!pluginEnv.stateDir || !pluginEnv.configDir) {
    log("no plugin state directory; nothing to clean up");
    return 0;
  }
  const config = loadConfig(pluginEnv.configDir, env);
  if (!config.cleanupOnWorktreeRemoved) {
    log("cleanupOnWorktreeRemoved is false; leaving the mappings alone");
    return 0;
  }
  const removedPath = removedWorktreePath(readEventPayload(env));
  if (!removedPath) {
    log("event carries no worktree path; nothing to clean up");
    return 0;
  }
  const matches = entriesForLocalPath(loadState(pluginEnv.stateDir), removedPath);
  if (matches.length === 0) {
    log(`no sandboxed agent mapped to ${removedPath}`);
    return 0;
  }
  const herdr = createHerdrClient({ bin: pluginEnv.herdrBin, env });
  const nono = createNonoClient({ bin: config.nonoBin, env });
  const lifecycle = createLifecycle({ stateDir: pluginEnv.stateDir, config, pluginRoot: pluginEnv.pluginRoot, nono, log, herdr, env });
  let failures = 0;
  const cleaned = [];
  for (const [paneId, entry] of matches) {
    try {
      if (bridgeIsRunning(entry)) {
        // The agent's working tree is gone; its session has nothing left to work on.
        await lifecycle.stop(paneId);
        log(`stopped ${entry.sessionName}`);
      }
      const removed = withPaneLock(pluginEnv.stateDir, paneId, () => {
        const now = getPaneEntry(pluginEnv.stateDir, paneId);
        if (now && (bridgeIsRunning(now) || shellIsRunning(now))) {
          return false;
        }
        return deletePaneEntry(pluginEnv.stateDir, paneId);
      });
      if (removed) {
        cleaned.push(entry.sessionName);
        log(`forgot mapping for pane ${paneId} (${entry.sessionName})`);
      } else {
        log(`kept mapping for pane ${paneId} (${entry.sessionName}): its agent or a shell still runs`);
      }
    } catch (error) {
      failures += 1;
      process.stderr.write(`could not clean up ${entry.sessionName} for pane ${paneId}: ${errorMessageOf(error)}\n`);
    }
  }
  if (cleaned.length > 0) {
    herdr.notify("nono sandboxes cleaned up", `${cleaned.join(", ")} (worktree ${removedPath} was removed)`);
  }
  return failures === 0 ? 0 : 1;
}

/**
 * Handles the event and returns the exit code. Any unexpected failure is
 * reported as one line on stderr instead of a stack trace.
 * @param {NodeJS.ProcessEnv} [env]
 * @returns {Promise<number>}
 */
export async function handleEvent(env = process.env) {
  try {
    return await handleEventUnsafe(env);
  } catch (error) {
    process.stderr.write(`worktree cleanup failed: ${errorMessageOf(error)}\n`);
    return 1;
  }
}
