#!/usr/bin/env node
const { spawnSync } = require("node:child_process");
const { existsSync } = require("node:fs");
const { join } = require("node:path");

const targets = {
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
  "linux-x64": "x86_64-unknown-linux-gnu",
  "linux-arm64": "aarch64-unknown-linux-gnu",
};
const target = targets[`${process.platform}-${process.arch}`];
const binary = target ? join(__dirname, "..", "vendor", target, "stem") : null;
if (!binary || !existsSync(binary)) {
  console.error(`stem: no prebuilt binary for ${process.platform} ${process.arch}; build from source with: cargo install --git https://github.com/proxysoul/stem stem`);
  process.exit(1);
}
const run = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });
if (run.error) {
  console.error(`stem: ${run.error.message}`);
  process.exit(1);
}
process.exit(run.status ?? 1);
