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
  const previousRoot = process.env.SystemRoot;
  process.env.LOCALAPPDATA = path.join(temp, "User's cache & [files]");
  process.env.SystemRoot = "C:\\Windows";
  t.mock.method(childProcess, "execFileSync", execute || (() => { throw new Error("Unexpected download"); }));
  delete require.cache[require.resolve("../bin/install")];
  t.after(() => {
    if (previous === undefined) delete process.env.LOCALAPPDATA;
    else process.env.LOCALAPPDATA = previous;
    if (previousRoot === undefined) delete process.env.SystemRoot;
    else process.env.SystemRoot = previousRoot;
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
    if (path.win32.basename(command) !== "powershell.exe") return;
    const names = complete ? ["ast-bro.exe", "ast-outline.exe", "sb.exe"] : ["ast-bro.exe"];
    for (const name of names) fs.writeFileSync(path.join(options.env.AST_BRO_DESTINATION, name), name);
  });
  assert.throws(() => api.downloadBinary());
  assert.deepEqual(fs.readdirSync(path.dirname(api.getBinaryPath())), []);
  complete = true;
  assert.equal(api.downloadBinary(), api.getBinaryPath());
});

/** Populate the staged files as if the release ZIP had been extracted. */
function extractedRelease(command, args, options) {
  if (path.win32.basename(command) !== "powershell.exe") return;
  for (const name of ["ast-bro.exe", "ast-outline.exe", "sb.exe"]) {
    fs.writeFileSync(path.join(options.env.AST_BRO_DESTINATION, name), "downloaded");
  }
}

test("reuses a competing installation completed during download", (t) => {
  const api = installer(t, "win32", "x64", (command, args, options) => {
    extractedRelease(command, args, options);
    if (path.win32.basename(command) !== "powershell.exe") return;
    for (const name of ["ast-bro.exe", "ast-outline.exe", "sb.exe"]) {
      fs.writeFileSync(path.join(path.dirname(api.getBinaryPath()), name), "winner");
    }
  });
  assert.equal(api.downloadBinary(), api.getBinaryPath());
  assert.equal(fs.readFileSync(api.getBinaryPath(), "utf8"), "winner");
});

test("reuses a running winner that appears between the check and rename", (t) => {
  const api = installer(t, "win32", "x64", extractedRelease);
  const rename = fs.renameSync;
  t.mock.method(fs, "renameSync", (source, destination) => {
    if (path.basename(destination) !== "ast-bro.exe") return rename(source, destination);
    const cache = path.dirname(destination);
    assert.ok(fs.existsSync(path.join(cache, "ast-outline.exe")));
    assert.ok(fs.existsSync(path.join(cache, "sb.exe")));
    fs.writeFileSync(destination, "running winner");
    throw Object.assign(new Error("Executable is running"), { code: "EPERM" });
  });
  assert.equal(api.downloadBinary(), api.getBinaryPath());
  assert.equal(fs.readFileSync(api.getBinaryPath(), "utf8"), "running winner");
  assert.equal(fs.readdirSync(path.dirname(api.getBinaryPath())).length, 3);
});

test("preserves publication errors when no competing file exists", (t) => {
  const api = installer(t, "win32", "x64", extractedRelease);
  const failure = Object.assign(new Error("Access denied"), { code: "EACCES" });
  t.mock.method(fs, "renameSync", () => { throw failure; });
  assert.throws(() => api.downloadBinary(), (error) => error === failure);
  assert.deepEqual(fs.readdirSync(path.dirname(api.getBinaryPath())), []);
});

test("uses Windows system utilities without searching the checkout or PATH", (t) => {
  const commands = [];
  const api = installer(t, "win32", "x64", (command, args, options) => {
    commands.push(command);
    extractedRelease(command, args, options);
  });
  process.env.SystemRoot = "D:\\Windows";
  api.downloadBinary();
  assert.deepEqual(commands, [
    "D:\\Windows\\System32\\curl.exe",
    "D:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
  ]);
});

test("rejects relative Windows system paths before executing a utility", (t) => {
  const api = installer(t, "win32", "x64");
  process.env.SystemRoot = "checkout";
  assert.throws(() => api.downloadBinary(), /absolute Windows path/);
  assert.deepEqual(fs.readdirSync(path.dirname(api.getBinaryPath())), []);
});

test("bounds curl downloads and permits retry after a timeout", (t) => {
  let timedOut = true;
  const api = installer(t, "win32", "x64", (command, args, options) => {
    if (path.win32.basename(command) === "curl.exe") {
      assert.equal(args[args.indexOf("--connect-timeout") + 1], "30");
      assert.equal(args[args.indexOf("--max-time") + 1], "120");
      if (timedOut) {
        fs.writeFileSync(args[args.indexOf("--output") + 1], "partial download");
        throw Object.assign(new Error("curl timed out"), { status: 28 });
      }
    }
    extractedRelease(command, args, options);
  });
  assert.throws(() => api.downloadBinary(), /curl timed out/);
  assert.deepEqual(fs.readdirSync(path.dirname(api.getBinaryPath())), []);
  timedOut = false;
  assert.equal(api.downloadBinary(), api.getBinaryPath());
});
