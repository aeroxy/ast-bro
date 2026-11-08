/** Installer regressions independent of the host operating system. */
const assert = require("node:assert/strict");
const { test } = require("node:test");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const childProcess = require("node:child_process");

/** Load the installer after stubbing process execution, without touching user caches. */
function installer(t, platform, arch, execute) {
  t.mock.method(os, "platform", () => platform);
  t.mock.method(os, "arch", () => arch);
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), "ast-bro-test-"));
  t.mock.method(os, "homedir", () => temp);
  const previous = process.env.LOCALAPPDATA;
  process.env.LOCALAPPDATA = path.join(temp, "User's cache & [files]");
  t.mock.method(childProcess, "execFileSync", execute || (() => { throw new Error("Unexpected download"); }));
  delete require.cache[require.resolve("../bin/install")];
  t.after(() => {
    if (previous === undefined) delete process.env.LOCALAPPDATA;
    else process.env.LOCALAPPDATA = previous;
    fs.rmSync(temp, { recursive: true, force: true });
  });
  return require("../bin/install");
}

test("selects the Windows x64 release and versioned .exe cache", (t) => {
  const api = installer(t, "win32", "x64");
  assert.deepEqual(api.getPlatform(), { osName: "windows", archName: "x86_64" });
  const binary = api.getBinaryPath();
  assert.equal(path.basename(binary), "ast-bro.exe");
  assert.ok(binary.startsWith(process.env.LOCALAPPDATA));
  assert.ok(binary.includes(require("../package.json").version));
  fs.writeFileSync(binary, "cached");
  assert.equal(api.downloadBinary(), binary);
});

test("retains macOS ARM64 support", (t) => {
  const api = installer(t, "darwin", "arm64");
  assert.deepEqual(api.getPlatform(), { osName: "macos", archName: "arm64" });
  assert.equal(path.basename(api.getBinaryPath()), "ast-bro");
});

test("rejects unshipped architectures before attempting a download", (t) => {
  const api = installer(t, "win32", "arm64");
  assert.throws(() => api.downloadBinary(), /No pre-built binary for windows-arm64/);
});

test("cleans up a failed download without caching a binary", (t) => {
  const api = installer(t, "win32", "x64", () => { throw new Error("HTTP 404"); });
  assert.throws(() => api.downloadBinary(), /HTTP 404/);
  assert.deepEqual(fs.readdirSync(path.dirname(api.getBinaryPath())), []);
});

test("rejects an incomplete archive and permits a later retry", (t) => {
  let complete = false;
  const api = installer(t, "win32", "x64", (command, args, options) => {
    if (command !== "powershell.exe") return;
    const names = complete ? ["ast-bro.exe", "ast-outline.exe", "sb.exe"] : ["ast-bro.exe"];
    for (const name of names) fs.writeFileSync(path.join(options.env.AST_BRO_DESTINATION, name), name);
  });
  assert.throws(() => api.downloadBinary());
  assert.deepEqual(fs.readdirSync(path.dirname(api.getBinaryPath())), []);
  complete = true;
  assert.equal(api.downloadBinary(), api.getBinaryPath());
});
