import { defineConfig } from "@playwright/test";

// Browser end-to-end tests against the real server binary (see e2e/server.ts).
// Uses the locally installed Chrome; no browser download is needed. E2E_CHROME_PATH runs a
// specific Chrome or Chromium binary instead (for machines without Google Chrome).
// E2E_BROWSER=firefox or webkit runs Playwright's Firefox or WebKit (the engine of Safari)
// instead; install them first with `npx playwright install firefox webkit`.
const chromePath = process.env.E2E_CHROME_PATH;
const browserName = (process.env.E2E_BROWSER ?? "chromium") as "chromium" | "firefox" | "webkit";
const chromium = chromePath
  ? { launchOptions: { executablePath: chromePath } }
  : { channel: process.env.E2E_BROWSER_CHANNEL ?? "chrome" };

export default defineConfig({
  testDir: "e2e",
  timeout: 120_000,
  expect: { timeout: 15_000 },
  workers: 1,
  reporter: [["list"]],
  use: {
    browserName,
    ...(browserName === "chromium" ? chromium : {}),
    headless: true,
    viewport: { width: 1280, height: 800 },
    trace: "retain-on-failure",
  },
});
