#!/usr/bin/env node

/**
 * ast-bro CLI wrapper — forwards arguments to the Rust binary.
 */

const { execFileSync } = require("child_process");
const { downloadBinary } = require("./install");
const args = process.argv.slice(2);

try {
  // Package managers may skip postinstall; first use can finish installation.
  const binary = downloadBinary();
  execFileSync(binary, args, { stdio: "inherit" });
} catch (err) {
  if (err.status !== undefined) {
    process.exit(err.status);
  }
  console.error(err.message);
  process.exit(1);
}
