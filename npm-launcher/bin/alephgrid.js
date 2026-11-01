#!/usr/bin/env node
// Runs the Alephgrid binary for this platform, installed by npm as an optional dependency
// (@alephgrid/cli-<platform>-<arch>). Arguments, input/output and the exit code pass through.
"use strict";
const { spawn } = require("node:child_process");

const pkg = `@alephgrid/cli-${process.platform}-${process.arch}`;
let bin;
try {
  bin = require.resolve(`${pkg}/bin/alephgrid`);
} catch {
  const hint = process.platform === "win32"
    ? "Alephgrid does not run natively on Windows yet; use Docker (docker run -p 7878:7878 ghcr.io/alephgrid/alephgrid) or WSL."
    : `No Alephgrid build for ${process.platform}-${process.arch}, or the optional dependency ${pkg} was not installed (npm install --include=optional).`;
  console.error(`alephgrid: ${hint}`);
  process.exit(1);
}

const child = spawn(bin, process.argv.slice(2), { stdio: "inherit" });
for (const sig of ["SIGINT", "SIGTERM", "SIGHUP"]) process.on(sig, () => child.kill(sig));
child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  else process.exit(code ?? 1);
});
child.on("error", (e) => {
  console.error(`alephgrid: could not start ${bin}: ${e.message}`);
  process.exit(1);
});
