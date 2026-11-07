// Build the npm packages for a release from its tarballs (docs/RELEASING.md):
//
//   node tools/release/npm.mjs VERSION DIST OUT
//
// DIST holds alephgrid-VERSION-<target>.tar.gz; OUT receives one folder per package:
//   cli-<platform>-<arch>/  alephgrid-<platform>-<arch>: the binary, for that os/cpu only
//                           (unscoped: no npm organization is needed)
//   alephgrid/              alephgrid: the `alephgrid` command, depending on all of them as
//                           optional dependencies, so npm installs only the matching one.
// One package serves npm, pnpm, yarn and bun. Publish the platform packages first.

import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const [version, dist, out] = process.argv.slice(2);
if (!version || !dist || !out) {
  console.error("usage: node tools/release/npm.mjs VERSION DIST OUT");
  process.exit(2);
}
const root = resolve(fileURLToPath(new URL("../..", import.meta.url)));
const PLATFORMS = [
  { target: "x86_64-unknown-linux-gnu", os: "linux", cpu: "x64", libc: "glibc" },
  { target: "aarch64-unknown-linux-gnu", os: "linux", cpu: "arm64", libc: "glibc" },
  { target: "aarch64-apple-darwin", os: "darwin", cpu: "arm64" },
  { target: "x86_64-apple-darwin", os: "darwin", cpu: "x64" },
];
// Shipped with every package that holds the binary (tools/licenses.py writes the notices).
const NOTICES = ["LICENSE", "NOTICE", "THIRD-PARTY-NOTICES.md"];
const common = {
  version,
  license: "Apache-2.0",
  homepage: "https://github.com/alephgrid/alephgrid",
  repository: { type: "git", url: "git+https://github.com/alephgrid/alephgrid.git" },
  bugs: "https://github.com/alephgrid/alephgrid/issues",
};

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });
const optional = {};
for (const p of PLATFORMS) {
  const tarball = join(dist, `alephgrid-${version}-${p.target}.tar.gz`);
  if (!existsSync(tarball)) throw new Error(`missing ${tarball}`);
  const name = `alephgrid-${p.os}-${p.cpu}`;
  const dir = join(out, `cli-${p.os}-${p.cpu}`);
  mkdirSync(join(dir, "bin"), { recursive: true });
  const tmp = mkdtempSync(join(tmpdir(), "alephgrid-npm-"));
  execFileSync("tar", ["-xzf", tarball, "-C", tmp]);
  copyFileSync(join(tmp, `alephgrid-${version}-${p.target}`, "alephgrid"), join(dir, "bin", "alephgrid"));
  chmodSync(join(dir, "bin", "alephgrid"), 0o755);
  rmSync(tmp, { recursive: true, force: true });
  for (const f of NOTICES) copyFileSync(join(root, f), join(dir, f));
  writeFileSync(join(dir, "README.md"), `# ${name}\n\nThe Alephgrid ${version} binary for ${p.os}-${p.cpu}. Install \`alephgrid\` instead; it picks this package for you.\n`);
  writeFileSync(join(dir, "package.json"), JSON.stringify({
    name, ...common,
    description: `Alephgrid binary for ${p.os}-${p.cpu}`,
    os: [p.os], cpu: [p.cpu], ...(p.libc ? { libc: [p.libc] } : {}),
    files: ["bin/alephgrid", ...NOTICES, "README.md"],
    preferUnplugged: true,
  }, null, 2) + "\n");
  optional[name] = version;
}

const main = join(out, "alephgrid");
mkdirSync(join(main, "bin"), { recursive: true });
copyFileSync(join(root, "npm-launcher", "bin", "alephgrid.js"), join(main, "bin", "alephgrid.js"));
chmodSync(join(main, "bin", "alephgrid.js"), 0o755);
copyFileSync(join(root, "LICENSE"), join(main, "LICENSE"));
copyFileSync(join(root, "npm-launcher", "README.md"), join(main, "README.md"));
writeFileSync(join(main, "package.json"), JSON.stringify({
  name: "alephgrid", ...common,
  description: "Open-source spreadsheet for data that doesn't fit in a spreadsheet: tens of millions of rows, Excel formulas, SQL when you want it.",
  keywords: ["spreadsheet", "duckdb", "big-data", "sql", "excel", "csv", "parquet", "self-hosted"],
  bin: { alephgrid: "bin/alephgrid.js" },
  files: ["bin/alephgrid.js", "LICENSE", "README.md"],
  engines: { node: ">=18" },
  optionalDependencies: optional,
}, null, 2) + "\n");
console.log(`npm packages for ${version} in ${out}: ${[...Object.keys(optional), "alephgrid"].join(", ")}`);
