// @ts-check
import js from "@eslint/js";
import globals from "globals";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import prettier from "eslint-config-prettier";

export default tseslint.config(
  // Build output, deps, and the generated design artefacts — never linted.
  {
    ignores: [
      "dist/**",
      "node_modules/**",
      "coverage/**",
      "design/cards/**",
      "design/prototype/**",
    ],
  },

  js.configs.recommended,

  // Type-aware linting. This is the whole point of choosing ESLint over a faster
  // syntax-only linter: rules like no-floating-promises need the type checker.
  ...tseslint.configs.recommendedTypeChecked,
  {
    files: ["**/*.{ts,tsx}"],
    languageOptions: {
      ecmaVersion: 2020,
      globals: { ...globals.browser, ...globals.es2020 },
      parserOptions: {
        // The TS "project service" resolves each file to its owning tsconfig automatically, so
        // widening `include` in tsconfig.json is all it takes to bring a new file under
        // type-aware rules — there is no second file list to keep in sync here.
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      // --- React ---
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "error",
      "react-refresh/only-export-components": ["error", { allowConstantExport: true }],

      // --- The type-aware rules that actually catch bugs in this codebase ---
      // Fire-and-forget writes and react-query mutations are the real risk surface here.
      "@typescript-eslint/no-floating-promises": "error",
      "@typescript-eslint/no-misused-promises": "error",
      "@typescript-eslint/await-thenable": "error",
      "@typescript-eslint/no-unnecessary-type-assertion": "error",
      "@typescript-eslint/require-await": "error",

      // tsc's noUnusedLocals/noUnusedParameters already covers this with different (stricter,
      // and already-passing) semantics. Off, rather than double-reporting the same line.
      "@typescript-eslint/no-unused-vars": "off",

      // There are zero `any` in src today. Keep it that way, loudly.
      "@typescript-eslint/no-explicit-any": "error",

      // Each remaining site gets a real guard or an inline disable with a reason.
      "@typescript-eslint/no-non-null-assertion": "error",

      // Deliberately NOT enabled — these are choices, not oversights. Counts are measured
      // against this tree; revisit any of them as its own PR:
      //   no-unnecessary-condition      strict; ~50-150 hits, this codebase is defensive on purpose
      //   no-confusing-void-expression  strict; dislikes statement-position `void fn()`, which is
      //                                 the idiomatic no-floating-promises fix we use below
      //   restrict-template-expressions strict; needs per-case judgement, low bug yield
      //   prefer-nullish-coalescing     stylistic; 118 `||` sites to triage by hand
      //   consistent-type-definitions   stylistic; would rewrite api/types.ts, a file the
      //                                 file-size ratchet has baselined — churn it and CI fails
    },
  },

  // Plain-JS config/build files: no TS program behind them, so type-aware rules must be off.
  {
    files: ["**/*.{js,mjs,cjs}"],
    extends: [tseslint.configs.disableTypeChecked],
    languageOptions: { globals: { ...globals.node } },
  },

  // Build tooling that runs in node, not the browser.
  {
    files: ["vite.config.ts", "tailwind.config.ts", "design/**/*.ts"],
    languageOptions: { globals: { ...globals.node } },
  },

  // The service worker runs in neither: `self`, `caches` and friends live in the
  // ServiceWorkerGlobalScope. It is plain JS and outside the TS program by design.
  {
    files: ["public/sw.js"],
    languageOptions: { globals: { ...globals.serviceworker } },
  },

  // MUST be last: switches off every stylistic rule Prettier owns, so the two never disagree.
  // (Prettier runs as its own step, not through eslint-plugin-prettier — that is 3-5x slower
  // and reports formatting as lint errors, which buries the real ones.)
  prettier,
);
