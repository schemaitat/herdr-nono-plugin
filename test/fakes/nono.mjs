#!/usr/bin/env node
/**
 * Fake `nono` CLI for tests. Logs every invocation (argv, cwd and the
 * environment variables the tests care about) as a JSON line to FAKE_NONO_LOG
 * and keeps sessions in FAKE_NONO_STATE. FAKE_NONO_FAIL lists subcommands that
 * fail (`version`, `ps`, `stop`, `profile`, `run`), each optionally with a mode
 * (`profile:not-found`). `run` records a session whose supervisor pid is this
 * process, runs the command after `--` with the inherited stdio, marks the
 * session exited and exits with the command's code; the escape probe script is
 * answered with FAKE_NONO_PROBE (JSON) instead of being run. `profile show`
 * prints FAKE_NONO_PROFILE_JSON or, by the name of the reference, a blocked
 * (client), provider-allowlist (server) or open profile.
 */
import { spawn } from "node:child_process";
import { appendFileSync, existsSync, readFileSync, writeFileSync } from "node:fs";

const argv = process.argv.slice(2);
const logFile = process.env.FAKE_NONO_LOG;
const stateFile = process.env.FAKE_NONO_STATE;
if (logFile) {
  const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => /^(HERDR_|SSH_AUTH_SOCK$|DBUS_SESSION_BUS_ADDRESS$|TMUX$|PS1$|FAKE_AGENT_MARK$)/.test(key)));
  appendFileSync(logFile, `${JSON.stringify({ argv, cwd: process.cwd(), env })}\n`);
}

function loadState() {
  return stateFile && existsSync(stateFile) ? JSON.parse(readFileSync(stateFile, "utf8")) : { sessions: [] };
}

function saveState(state) {
  if (stateFile) writeFileSync(stateFile, JSON.stringify(state, null, 2));
}

function failureFor(subcommand) {
  for (const rule of (process.env.FAKE_NONO_FAIL ?? "").split(",").map((item) => item.trim()).filter(Boolean)) {
    const [name, mode = "generic"] = rule.split(":");
    if (name === subcommand) return mode;
  }
  return null;
}

function fail(mode, detail = "") {
  const messages = {
    "not-found": `nono: Session not found: ${detail}`,
    profile: `nono: Profile not found: ${detail}`,
    generic: "nono: something went wrong",
  };
  process.stderr.write(`${messages[mode] ?? messages.generic}\n`);
  process.exit(1);
}

const [command, ...rest] = argv;

if (command === "--version") {
  if (failureFor("version")) fail("generic");
  process.stdout.write(`nono ${process.env.FAKE_NONO_VERSION ?? "0.78.0"}\n`);
} else if (command === "ps") {
  if (failureFor("ps")) fail("generic");
  const all = rest.includes("--all");
  const sessions = loadState().sessions.filter((session) => all || session.status !== "exited");
  process.stdout.write(`${JSON.stringify(sessions, null, 2)}\n`);
} else if (command === "stop") {
  const id = rest.filter((item) => !item.startsWith("--"))[0];
  if (failureFor("stop")) fail("generic");
  const state = loadState();
  const session = state.sessions.find((item) => item.session_id.startsWith(id) && item.status !== "exited");
  if (!session) fail("not-found", id);
  session.status = "exited";
  saveState(state);
  try {
    process.kill(session.supervisor_pid, "SIGTERM");
  } catch {
    // Already gone.
  }
  process.stdout.write(`Stopped session ${session.session_id}.\n`);
} else if (command === "profile" && rest[0] === "show") {
  const ref = rest[rest.length - 1];
  const mode = failureFor("profile");
  if (mode) fail(mode === "not-found" ? "profile" : "generic", ref);
  // Like the shipped profiles: the client's blocks the network, the server's
  // goes through the proxy to provider hosts only, a "wildcard" one allows
  // every domain, anything else is open.
  const network = /wildcard/.test(ref) ? { allow_domain: ["*"] } : /client/.test(ref) ? { block: true } : /server/.test(ref) ? { allow_domain: ["models.opencode.ai", "github.com", "api.github.com", "api.githubcopilot.com", "*.githubcopilot.com"] } : { block: false, allow_domain: [] };
  const profile = process.env.FAKE_NONO_PROFILE_JSON
    ? JSON.parse(process.env.FAKE_NONO_PROFILE_JSON)
    : { name: ref, extends: ["nolabs-ai/opencode"], linux: { af_unix_mediation: "pathname" }, network, workdir: { access: "readwrite" } };
  process.stdout.write(`${JSON.stringify(profile, null, 2)}\n`);
} else if (command === "run") {
  if (failureFor("run")) fail("generic");
  const separator = argv.indexOf("--");
  const inner = argv.slice(separator + 1);
  const nameIndex = argv.indexOf("--name");
  const name = nameIndex === -1 ? null : argv[nameIndex + 1];
  if (inner.join(" ").includes("HERDR_NONO_PROBE")) {
    const probe = process.env.FAKE_NONO_PROBE
      ? JSON.parse(process.env.FAKE_NONO_PROBE)
      : { herdrSocket: "denied", opencodeServicePort: "denied", loopbackCanary: "denied", loopbackViaProxy: "denied", systemdUser: "denied", sessionBus: "denied", sshAgent: "absent", gpgAgent: "absent", dockerSocket: "denied", opencodeServicePassword: "absent", sshKeys: "denied", homeDirectory: "denied", env: [], marker: true };
    process.stdout.write(`HERDR_NONO_PROBE ${JSON.stringify(probe)}\n`);
    process.exit(0);
  }
  const id = `${process.pid.toString(16).padStart(8, "0")}${Date.now().toString(16).slice(-8)}`;
  const state = loadState();
  state.sessions.push({ session_id: id, name, supervisor_pid: process.pid, child_pid: null, status: "running", attachment: "attached", exit_code: null, command: inner, profile: argv[argv.indexOf("--profile") + 1], workdir: process.cwd(), network: "allowed" });
  saveState(state);
  const finish = (code) => {
    const now = loadState();
    const session = now.sessions.find((item) => item.session_id === id);
    if (session) {
      session.status = "exited";
      session.exit_code = code;
      saveState(now);
    }
    process.exit(code);
  };
  const child = spawn(inner[0], inner.slice(1), { stdio: "inherit" });
  child.on("error", (error) => {
    process.stderr.write(`nono: failed to execute ${inner[0]}: ${error.message}\n`);
    finish(127);
  });
  for (const signal of ["SIGTERM", "SIGINT", "SIGHUP"]) {
    process.on(signal, () => child.kill(signal));
  }
  child.on("exit", (code, signal) => finish(code ?? (signal === "SIGINT" ? 130 : 143)));
} else {
  process.stderr.write(`fake nono: unsupported command ${argv.join(" ")}\n`);
  process.exit(2);
}
