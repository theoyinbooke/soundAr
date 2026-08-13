import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/e2e",
  timeout: 30_000,
  use: {
    baseURL: "http://127.0.0.1:4173",
    screenshot: "only-on-failure",
  },
  webServer: {
    command: "npm run dev -- --host 127.0.0.1 --port 4173",
    port: 4173,
    reuseExistingServer: false,
  },
  projects: [
    { name: "narrow-phone", use: { viewport: { width: 320, height: 720 } } },
    { name: "phone", use: { viewport: { width: 390, height: 844 } } },
    { name: "compact", use: { viewport: { width: 820, height: 620 } } },
    { name: "medium", use: { viewport: { width: 1024, height: 768 } } },
    { name: "windowed", use: { viewport: { width: 1220, height: 720 } } },
    { name: "desktop", use: { viewport: { width: 1440, height: 900 } } },
  ],
});
