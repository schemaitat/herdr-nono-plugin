/**
 * Read-only views of the Linux process table (`/proc`), used to prove that an
 * agent and everything it started really run inside the nono sandbox. Every
 * reader tolerates processes that exit mid-scan and fields the kernel refuses
 * to show; unknown values are `null`, never guessed.
 * @module procfs
 */
import { existsSync, readdirSync, readFileSync, readlinkSync } from "node:fs";
import path from "node:path";

/**
 * One process as the verification sees it.
 * @typedef {object} ProcessInfo
 * @property {number} pid
 * @property {number|null} ppid
 * @property {string[]} argv
 * @property {boolean|null} noNewPrivs `no_new_privs`, which nono sets before it applies Landlock.
 * @property {number|null} seccomp The `Seccomp` mode from /proc/PID/status.
 * @property {boolean|null} nonoCapFile Whether the environment carries `NONO_CAP_FILE`, the marker nono gives sandboxed processes.
 */

/**
 * Whether this machine exposes a Linux-style /proc.
 * @param {string} [root]
 * @returns {boolean}
 */
export function procfsAvailable(root = "/proc") {
  return existsSync(path.join(root, "self", "status"));
}

function readText(file) {
  try {
    return readFileSync(file, "utf8");
  } catch {
    return null;
  }
}

/**
 * Parses the parent pid out of /proc/PID/stat. The command name may contain
 * spaces and parentheses, so fields are counted after the last `)`.
 * @param {string} stat
 * @returns {number|null}
 */
export function parentFromStat(stat) {
  const rest = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
  const ppid = Number(rest[1]);
  return Number.isInteger(ppid) && ppid >= 0 ? ppid : null;
}

/**
 * Reads one process, or null when it is gone.
 * @param {number} pid
 * @param {{root?: string}} [options]
 * @returns {ProcessInfo|null}
 */
export function readProcess(pid, { root = "/proc" } = {}) {
  const dir = path.join(root, String(pid));
  const stat = readText(path.join(dir, "stat"));
  if (stat === null) {
    return null;
  }
  const status = readText(path.join(dir, "status")) ?? "";
  const cmdline = readText(path.join(dir, "cmdline"));
  const environ = readText(path.join(dir, "environ"));
  const field = (name) => {
    const match = status.match(new RegExp(`^${name}:\\s*(\\d+)`, "m"));
    return match ? Number(match[1]) : null;
  };
  const noNewPrivs = field("NoNewPrivs");
  // cmdline is NUL-terminated, so splitting leaves one empty word at the end.
  const argv = cmdline === null ? [] : cmdline.split("\0");
  if (argv.at(-1) === "") argv.pop();
  return {
    pid,
    ppid: parentFromStat(stat),
    argv,
    noNewPrivs: noNewPrivs === null ? null : noNewPrivs === 1,
    seccomp: field("Seccomp"),
    nonoCapFile: environ === null ? null : environ.split("\0").some((entry) => entry.startsWith("NONO_CAP_FILE=")),
  };
}

/**
 * Lists every process id currently in /proc.
 * @param {{root?: string}} [options]
 * @returns {number[]}
 */
export function listPids({ root = "/proc" } = {}) {
  try {
    return readdirSync(root).filter((name) => /^\d+$/.test(name)).map(Number);
  } catch {
    return [];
  }
}

/**
 * Every descendant of `rootPid` (not including it), parents before children.
 * @param {number} rootPid
 * @param {{root?: string}} [options]
 * @returns {ProcessInfo[]}
 */
export function descendants(rootPid, { root = "/proc" } = {}) {
  const children = new Map();
  for (const pid of listPids({ root })) {
    const stat = readText(path.join(root, String(pid), "stat"));
    const ppid = stat === null ? null : parentFromStat(stat);
    if (ppid !== null) {
      if (!children.has(ppid)) children.set(ppid, []);
      children.get(ppid).push(pid);
    }
  }
  const found = [];
  const queue = [...(children.get(rootPid) ?? [])];
  const seen = new Set([rootPid]);
  while (queue.length > 0) {
    const pid = /** @type {number} */ (queue.shift());
    if (seen.has(pid)) continue;
    seen.add(pid);
    const info = readProcess(pid, { root });
    if (info) {
      found.push(info);
      queue.push(...(children.get(pid) ?? []));
    }
  }
  return found;
}

/**
 * Socket inodes a process holds open, or null when its fds cannot be read.
 * @param {number} pid
 * @param {{root?: string}} [options]
 * @returns {Set<string>|null}
 */
export function socketInodes(pid, { root = "/proc" } = {}) {
  const dir = path.join(root, String(pid), "fd");
  let names;
  try {
    names = readdirSync(dir);
  } catch {
    return null;
  }
  const inodes = new Set();
  for (const name of names) {
    try {
      const match = readlinkSync(path.join(dir, name)).match(/^socket:\[(\d+)\]$/);
      if (match) inodes.add(match[1]);
    } catch {
      // The fd closed between the listing and the readlink.
    }
  }
  return inodes;
}

/**
 * Decodes the hex `address:port` of /proc/net/tcp{,6}.
 * @param {string} hex
 * @returns {{address: string, port: number}}
 */
export function decodeTcpAddress(hex) {
  const [addressHex, portHex] = hex.split(":");
  const port = Number.parseInt(portHex, 16);
  if (addressHex.length === 8) {
    const bytes = addressHex.match(/../g).map((byte) => Number.parseInt(byte, 16)).reverse();
    return { address: bytes.join("."), port };
  }
  // IPv6: four little-endian 32-bit words.
  const words = addressHex.toLowerCase().match(/.{8}/g).map((word) => word.match(/../g).reverse().join(""));
  const groups = words.join("").match(/.{4}/g).map((group) => group.replace(/^0+(?=.)/, ""));
  const text = groups.join(":");
  const mapped = text.match(/^0:0:0:0:0:ffff:([0-9a-f]+):([0-9a-f]+)$/);
  if (mapped) {
    const high = Number.parseInt(mapped[1], 16);
    const low = Number.parseInt(mapped[2], 16);
    return { address: [high >> 8, high & 255, low >> 8, low & 255].join("."), port };
  }
  return { address: text === "0:0:0:0:0:0:0:1" ? "::1" : text, port };
}

/**
 * TCP connections by socket inode, from /proc/net/tcp and tcp6 (the network
 * namespace of the reading process, which nono shares with the host).
 * @param {{root?: string}} [options]
 * @returns {Map<string, {local: {address: string, port: number}, remote: {address: string, port: number}, state: string}>}
 */
export function tcpConnections({ root = "/proc" } = {}) {
  const byInode = new Map();
  for (const file of ["tcp", "tcp6"]) {
    const text = readText(path.join(root, "net", file));
    if (text === null) continue;
    for (const line of text.split("\n").slice(1)) {
      const parts = line.trim().split(/\s+/);
      if (parts.length < 10) continue;
      byInode.set(parts[9], { local: decodeTcpAddress(parts[1]), remote: decodeTcpAddress(parts[2]), state: parts[3] });
    }
  }
  return byInode;
}

/** TCP state code of an established connection in /proc/net/tcp. */
export const TCP_ESTABLISHED = "01";

/** TCP state code of a listening socket in /proc/net/tcp. */
export const TCP_LISTEN = "0A";
