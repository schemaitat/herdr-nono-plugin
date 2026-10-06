import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { arch, tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";
import { ROOT } from "./helpers.mjs";

const VERSION = "9.9.9";
const TARGET = arch() === "arm64" ? "aarch64-unknown-linux-musl" : "x86_64-unknown-linux-musl";
const SCRIPT = path.join(ROOT, "scripts", "install-binary.sh");

/** The external programs the script uses; nothing else (no curl, no cargo) is on the test PATH. */
const TOOLS = ["sed", "head", "cut", "uname", "mktemp", "mkdir", "cp", "mv", "chmod", "rm", "sha256sum", "cat", "sh"];

function which(tool) {
  const found = spawnSync("sh", ["-c", `command -v ${tool}`], { encoding: "utf8" }).stdout.trim();
  assert.ok(found, `${tool} is needed by the test`);
  return found;
}

function executable(file, body) {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, `#!/bin/sh\n${body}\n`);
  chmodSync(file, 0o755);
  return file;
}

/** A fake plugin binary that answers --version like the real one. */
const fakeBinary = (version = VERSION, extra = "") => `${extra}\n[ "$1" = "--version" ] && echo "herdr-nono ${version}"\nexit 0`;

/** A plugin root with the script and a manifest, a release directory, and a PATH of just the tools. */
function setup({ assets = true, binary = fakeBinary() } = {}) {
  const dir = mkdtempSync(path.join(tmpdir(), "herdr-nono-install-"));
  const root = path.join(dir, "plugin");
  mkdirSync(path.join(root, "scripts"), { recursive: true });
  copyFileSync(SCRIPT, path.join(root, "scripts", "install-binary.sh"));
  writeFileSync(path.join(root, "herdr-plugin.toml"), `id = "nono.sandbox"\nversion = "${VERSION}"\nmin_herdr_version = "0.9.0"\n`);
  const release = path.join(dir, "release");
  mkdirSync(release);
  const tools = path.join(dir, "tools");
  mkdirSync(tools);
  for (const tool of TOOLS) symlinkSync(which(tool), path.join(tools, tool));
  const asset = `herdr-nono-${VERSION}-${TARGET}`;
  if (assets) {
    executable(path.join(release, asset), binary);
    writeFileSync(path.join(release, "SHA256SUMS"), `${createHash("sha256").update(readFileSync(path.join(release, asset))).digest("hex")}  ${asset}\n`);
  }
  const home = path.join(dir, "home");
  mkdirSync(home);
  return {
    dir,
    root,
    release,
    tools,
    asset,
    home,
    binary: path.join(root, "bin", "herdr-nono"),
    run(env = {}) {
      return spawnSync("/bin/sh", [path.join(root, "scripts", "install-binary.sh")], { encoding: "utf8", env: { PATH: tools, HOME: home, HERDR_NONO_RELEASE_DIR: release, ...env } });
    },
  };
}

/** A fake cargo that "builds" the plugin binary into target/release. */
function fakeCargo(s, binary = fakeBinary()) {
  const dir = path.join(s.dir, "cargo-bin");
  executable(path.join(dir, "cargo"), `mkdir -p target/release\ncat > target/release/herdr-nono <<'FAKE'\n#!/bin/sh\n${binary}\nFAKE\nchmod 755 target/release/herdr-nono\necho "$@" > "${s.dir}/cargo-args"`);
  return dir;
}

test("it installs the release asset after checking its checksum, and leaves a current install alone", () => {
  const s = setup();
  const first = s.run();
  assert.equal(first.status, 0, first.stderr);
  assert.match(first.stderr, new RegExp(`installed bin/herdr-nono ${VERSION} \\(release ${TARGET}\\)`));
  assert.ok(statSync(s.binary).mode & 0o100, "the binary is executable");
  assert.equal(spawnSync(s.binary, ["--version"], { encoding: "utf8" }).stdout.trim(), `herdr-nono ${VERSION}`);
  const before = statSync(s.binary).mtimeMs;
  const again = s.run();
  assert.equal(again.status, 0, again.stderr);
  assert.match(again.stderr, /already installed/);
  assert.equal(statSync(s.binary).mtimeMs, before, "a current install is not rewritten");
});

test("a binary of another version is replaced", () => {
  const s = setup();
  executable(s.binary, fakeBinary("0.0.1"));
  const result = s.run();
  assert.equal(result.status, 0, result.stderr);
  assert.equal(spawnSync(s.binary, ["--version"], { encoding: "utf8" }).stdout.trim(), `herdr-nono ${VERSION}`);
});

test("a checksum mismatch is refused, and without cargo nothing is installed", () => {
  const s = setup();
  writeFileSync(path.join(s.release, "SHA256SUMS"), `${"0".repeat(64)}  ${s.asset}\n`);
  const result = s.run();
  assert.equal(result.status, 1);
  assert.match(result.stderr, /checksum of .* does not match SHA256SUMS/);
  assert.match(result.stderr, /could not install the plugin binary/);
  assert.match(result.stderr, /releases\/tag\/v9\.9\.9/, "the manual route is explained");
  assert.ok(!existsSync(s.binary), "a binary that failed verification is never installed");
});

test("without a matching release it builds from source with cargo", () => {
  const s = setup({ assets: false });
  const cargo = fakeCargo(s);
  const result = s.run({ PATH: `${s.tools}:${cargo}` });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stderr, /no release asset/);
  assert.match(result.stderr, /built from source/);
  assert.equal(readFileSync(path.join(s.dir, "cargo-args"), "utf8").trim(), "build --release --locked");
  assert.equal(spawnSync(s.binary, ["--version"], { encoding: "utf8" }).stdout.trim(), `herdr-nono ${VERSION}`);
});

test("cargo in ~/.cargo/bin is found even when it is not on PATH", () => {
  const s = setup({ assets: false });
  const cargo = fakeCargo(s);
  mkdirSync(path.join(s.home, ".cargo"), { recursive: true });
  symlinkSync(cargo, path.join(s.home, ".cargo", "bin"));
  const result = s.run();
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stderr, /built from source/);
});

test("HERDR_NONO_NO_BUILD stops the fallback", () => {
  const s = setup({ assets: false });
  const cargo = fakeCargo(s);
  const result = s.run({ PATH: `${s.tools}:${cargo}`, HERDR_NONO_NO_BUILD: "1" });
  assert.equal(result.status, 1);
  assert.ok(!existsSync(path.join(s.dir, "cargo-args")), "cargo was not run");
  assert.ok(!existsSync(s.binary));
});

test("a downloaded binary that does not run here falls back to the build", () => {
  const s = setup({ binary: "exit 3" });
  const cargo = fakeCargo(s);
  const result = s.run({ PATH: `${s.tools}:${cargo}` });
  assert.match(result.stderr, /does not run on this machine/);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stderr, /built from source/);
});

test("a system that is not Linux is refused before anything is downloaded", () => {
  const s = setup();
  executable(path.join(s.dir, "fake", "uname"), 'echo Darwin');
  const result = s.run({ PATH: `${path.join(s.dir, "fake")}:${s.tools}` });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /unsupported system Darwin: the plugin runs on Linux only/);
  assert.ok(!existsSync(s.binary));
});

test("an architecture without releases still builds from source", () => {
  const s = setup();
  executable(path.join(s.dir, "fake", "uname"), 'if [ "$1" = "-m" ]; then echo riscv64; else echo Linux; fi');
  const cargo = fakeCargo(s);
  const result = s.run({ PATH: `${path.join(s.dir, "fake")}:${s.tools}:${cargo}` });
  assert.match(result.stderr, /unsupported architecture riscv64/);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stderr, /built from source/);
});

const CURL = spawnSync("sh", ["-c", "command -v curl"], { encoding: "utf8" }).stdout.trim();

/** Runs the script without blocking the event loop, so an in-process HTTP server can answer it. */
async function installAsync(s, env) {
  const child = spawn("/bin/sh", [path.join(s.root, "scripts", "install-binary.sh")], { env: { PATH: s.tools, HOME: s.home, ...env }, stdio: ["ignore", "pipe", "pipe"] });
  let stderr = "";
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  const status = await new Promise((resolve) => child.once("close", resolve));
  return { status, stderr };
}

test("it downloads the assets over HTTP with curl", { skip: CURL ? false : "curl is not installed" }, async () => {
  const s = setup();
  symlinkSync(CURL, path.join(s.tools, "curl"));
  const requests = [];
  const server = createServer((request, response) => {
    requests.push(request.url);
    const file = path.join(s.release, path.basename(request.url));
    if (!existsSync(file) || !request.url.includes(`/v${VERSION}/`)) {
      response.statusCode = 404;
      response.end("not found");
      return;
    }
    response.end(readFileSync(file));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = /** @type {import("node:net").AddressInfo} */ (server.address());
  try {
    const ok = await installAsync(s, { HERDR_NONO_RELEASE_URL: `http://127.0.0.1:${port}/releases/v${VERSION}` });
    assert.equal(ok.status, 0, ok.stderr);
    assert.deepEqual(requests, [`/releases/v${VERSION}/${s.asset}`, `/releases/v${VERSION}/SHA256SUMS`]);
    assert.equal(spawnSync(s.binary, ["--version"], { encoding: "utf8" }).stdout.trim(), `herdr-nono ${VERSION}`);
    // A 404 is the unreleased-checkout case: no asset, no build tool, a clear failure.
    const empty = setup({ assets: false });
    symlinkSync(CURL, path.join(empty.tools, "curl"));
    const missing = await installAsync(empty, { HERDR_NONO_RELEASE_URL: `http://127.0.0.1:${port}/releases/none` });
    assert.equal(missing.status, 1);
    assert.match(missing.stderr, /no release asset/);
    assert.ok(!existsSync(empty.binary));
  } finally {
    server.close();
  }
});
