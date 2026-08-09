import { fileURLToPath, URL } from "node:url";
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

function manualChunks(id: string) {
  if (!id.includes("/node_modules/")) return undefined;
  if (id.includes("/@blocknote/") || id.includes("/prosemirror-") || id.includes("/@tiptap/")) {
    return "editor";
  }
  if (
    id.includes("/react-markdown/") ||
    id.includes("/remark-") ||
    id.includes("/micromark") ||
    id.includes("/unified/") ||
    id.includes("/vfile") ||
    id.includes("/mdast-") ||
    id.includes("/hast-") ||
    id.includes("/unist-")
  ) {
    return "markdown";
  }
  if (id.includes("/highlight.js/")) return "highlight";
  if (id.includes("/uplot/")) return "charts";
  return undefined;
}

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
    // The only large chunk left is the lazy-loaded BlockNote editor. Splitting its mutually
    // dependent packages creates Rollup chunk cycles, so keep it together and keep the warning
    // threshold just above that intentional async payload.
    chunkSizeWarningLimit: 1_100,
    rollupOptions: {
      output: {
        manualChunks,
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
