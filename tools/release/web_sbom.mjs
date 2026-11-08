// CycloneDX SBOM for the web bundle embedded in the binary.
//
//   node tools/release/web_sbom.mjs < npm-sbom.json > alephgrid-VERSION-web.cdx.json
//
// Reads `npm sbom --sbom-format cyclonedx --omit dev` (the bundle's production npm
// dependencies, none today) and adds what the bundle ships that npm does not list as a
// production dependency, the same two entries as THIRD-PARTY-NOTICES.md: vite's
// module-preload helper, which vite inserts into the bundle (version from
// web/package-lock.json), and the Manrope font file.
import { readFileSync } from "node:fs";

const root = new URL("../../", import.meta.url);
const bom = JSON.parse(readFileSync(0, "utf8"));
if (bom.bomFormat !== "CycloneDX") throw new Error("input is not a CycloneDX SBOM");

const lock = JSON.parse(readFileSync(new URL("web/package-lock.json", root), "utf8"));
const vite = lock.packages?.["node_modules/vite"];
if (!vite?.version) throw new Error("vite is not in web/package-lock.json");
readFileSync(new URL("web/public/fonts/Manrope-variable.woff2", root)); // must exist

bom.components ??= [];
const added = [
  {
    type: "library",
    "bom-ref": `vite@${vite.version}`,
    name: "vite",
    version: vite.version,
    description: "module-preload helper inserted into the bundle",
    licenses: [{ license: { id: "MIT" } }],
    purl: `pkg:npm/vite@${vite.version}`,
  },
  {
    type: "file",
    "bom-ref": "web/public/fonts/Manrope-variable.woff2",
    name: "Manrope-variable.woff2",
    description: "Manrope font",
    licenses: [{ license: { id: "OFL-1.1" } }],
  },
];
for (const c of added) {
  if (!bom.components.some((x) => x["bom-ref"] === c["bom-ref"])) bom.components.push(c);
}
process.stdout.write(JSON.stringify(bom, null, 2) + "\n");
