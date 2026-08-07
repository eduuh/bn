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
      // The live agent terminal streams over `GET /ws/agents/:target/terminal`, which is not an
      // `/api` path — without an explicit websocket proxy the terminal sits on "connecting…"
      // forever in `npm run dev` and can only be exercised against the built binary.
      "/ws": { target: "ws://localhost:7770", ws: true },
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
    rollupOptions: {
      output: {
        // The framework never changes between dashboard builds while app code changes every
        // release, so keeping React/router/query in their own chunk means a rebuild only
        // invalidates the app chunk in the browser (and in the service worker's cache).
        manualChunks: {
          vendor: ["react", "react-dom", "react-router-dom", "@tanstack/react-query"],
        },
      },
    },
  },
  test: {
    // Unit tests. a055f1c had deleted every `*.test.ts(x)` along with the Playwright suite, leaving
    // this lane green vacuously; it is live again as of the `useAppMutation` write layer, whose
    // rollback-vs-SSE race is the kind of thing that only stays correct if something checks.
    // jsdom because react-query's hooks need a DOM to render into.
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
