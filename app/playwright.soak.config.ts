import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/soak",
  timeout: 120_000,
  workers: 1,
  retries: 0,
  use: {
    baseURL: "http://127.0.0.1:4173",
    screenshot: "only-on-failure",
    viewport: { width: 1220, height: 720 },
  },
  webServer: {
    command: "npm run dev -- --host 127.0.0.1 --port 4173",
    port: 4173,
    reuseExistingServer: false,
  },
});
