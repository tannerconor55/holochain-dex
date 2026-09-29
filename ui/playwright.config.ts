import { defineConfig } from "@playwright/test";

// Run inside the dev shell after ./build.sh:  nix develop .. -c npx playwright test
export default defineConfig({
  testDir: "e2e",
  timeout: 6 * 60_000,
  expect: { timeout: 90_000 },
  workers: 1,
  reporter: [["list"]],
  use: { baseURL: "http://localhost:8888", trace: "retain-on-failure" },
  webServer: { command: "npm run dev", port: 8888, reuseExistingServer: true },
});
