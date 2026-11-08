/** Package installation is best effort; invoking an unavailable CLI must fail. */
const assert = require("node:assert/strict");
const { test } = require("node:test");
const { spawnSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

for (const [platform, arch, error] of [
  ["linux", "x64", /No pre-built binary for linux-x86_64/],
  ["darwin", "x64", /No pre-built binary for macos-x86_64/],
  ["win32", "x64", /download stalled/],
]) {
  test(`postinstall warns but the launcher fails on ${platform}/${arch}`, (t) => {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), "ast-bro-postinstall-"));
    t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
    const hook = path.join(temp, "isolate.cjs");
    fs.writeFileSync(hook, `
      const os = require("node:os");
      os.platform = () => ${JSON.stringify(platform)};
      os.arch = () => ${JSON.stringify(arch)};
      os.homedir = () => process.env.LOCALAPPDATA;
      require("node:child_process").execFileSync = () => {
        process.stderr.write("download stalled\\n");
        throw Object.assign(new Error("download stalled"), { status: 28 });
      };
    `);
    const env = { ...process.env, LOCALAPPDATA: path.join(temp, "cache") };
    for (const entry of ["install", "ast-bro"]) {
      const result = spawnSync(process.execPath, [
        "--require", hook, require.resolve(`../bin/${entry}`), "--version",
      ], { env, encoding: "utf8", timeout: 10000 });
      assert.ifError(result.error);
      assert.equal(result.signal, null);
      if (entry === "install") assert.equal(result.status, 0, result.stderr);
      else assert.notEqual(result.status, 0, "An unavailable CLI must not report success");
      assert.equal(result.stdout, "");
      assert.match(result.stderr, error);
    }
  });
}
