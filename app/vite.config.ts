import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import packageMetadata from "./package.json";

export default defineConfig({
  plugins: [react()],
  define: {
    __APP_VERSION__: JSON.stringify(packageMetadata.version),
  },
  clearScreen: false,
  server: {
    port: 1421,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/target/**", "**/test-results/**", "**/playwright-report/**"],
    },
  },
});
