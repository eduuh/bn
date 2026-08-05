import { fileURLToPath, URL } from "node:url";
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Absolute base ("/") so built asset URLs are `/assets/...` regardless of the SPA route the shell
// was served at — bn-web embeds `dist/` and serves it from the root via rust-embed.
export default defineConfig({
  plugins: [react()],
  base: "/",
  server: {
    proxy: {
      "/api": "http://localhost:7770",
      "/events": "http://localhost:7770",
    },
  },
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  test: {
    // Unit tests only (`src/**/*.test.ts`). The Playwright E2E specs under `e2e/` are driven by
    // `npm run e2e` (playwright.config.ts), not vitest — vitest can't host `@playwright/test`.
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
