import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/production",
  timeout: 15_000,
  use: {
    baseURL: "http://127.0.0.1:4174",
    viewport: { width: 1220, height: 720 },
  },
  webServer: {
    command: "npm run preview -- --port 4174",
    port: 4174,
    reuseExistingServer: false,
  },
});
