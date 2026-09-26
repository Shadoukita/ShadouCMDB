import { defineConfig, devices } from "@playwright/test";
import { STORAGE_STATE } from "./e2e/global-setup";

// End-to-end walk of the UI against a real ShadouCMDB API (demo seed recommended).
//   E2E_BASE_URL=http://localhost:4173 npm run test:e2e -w frontend       # an already-running UI
//   API_PROXY_TARGET=http://<api-host>:3000 npm run test:e2e -w frontend  # starts `vite` itself, proxying /api
// The tests create their own uniquely named CIs and classes, so they can run against a shared database.
// They run signed in (see e2e/global-setup.ts: E2E_USERNAME / E2E_PASSWORD, or first-run setup on an empty database).
const baseURL = process.env.E2E_BASE_URL;
const proxyTarget = process.env.API_PROXY_TARGET;
if (!baseURL && !proxyTarget) {
  throw new Error("Set E2E_BASE_URL (a running UI) or API_PROXY_TARGET (the API to proxy a dev server to).");
}

export default defineConfig({
  testDir: "e2e",
  globalSetup: "./e2e/global-setup.ts",
  // The specs build on each other's data (create → edit → relate → delete), so run them in order.
  fullyParallel: false,
  workers: 1,
  retries: 0,
  forbidOnly: !!process.env.CI,
  // In CI the HTML report (with traces of failed tests) is uploaded as an artifact.
  reporter: process.env.CI ? [["list"], ["html", { open: "never" }]] : [["list"]],
  use: {
    baseURL: baseURL ?? "http://localhost:5199",
    storageState: STORAGE_STATE,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1440, height: 900 } } }],
  webServer: baseURL
    ? undefined
    : { command: "npx vite --port 5199 --strictPort", url: "http://localhost:5199", reuseExistingServer: false },
});
