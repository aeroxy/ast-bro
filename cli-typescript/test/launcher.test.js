/** Launcher failures must never report success without running the Rust command. */
const assert = require("node:assert/strict");
const { test } = require("node:test");
const { spawnSync } = require("node:child_process");

for (const stage of ["install", "command"]) {
  for (const status of [null, 2, 28]) {
    test(`forwards ${stage} failure with status ${status}`, () => {
      const script = `
        const install = require(${JSON.stringify(require.resolve("../bin/install"))});
        const failure = Object.assign(new Error("subprocess failed"), { status: ${status} });
        install.downloadBinary = () => {
          if (${JSON.stringify(stage)} === "install") throw failure;
          return "unused-binary";
        };
        require("node:child_process").execFileSync = () => { throw failure; };
        require(${JSON.stringify(require.resolve("../bin/ast-bro"))});
      `;
      const result = spawnSync(process.execPath, ["-e", script], { encoding: "utf8" });
      assert.equal(result.status, status === null ? 1 : status, result.stderr);
      assert.equal(result.stdout, "");
      if (status === null) assert.match(result.stderr, /subprocess failed/);
    });
  }
}
