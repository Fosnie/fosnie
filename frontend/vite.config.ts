import { defineConfig } from "vite";
import react, { reactCompilerPreset } from "@vitejs/plugin-react";
import babel from "@rolldown/plugin-babel";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath, URL } from "node:url";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

// Build-time release stamp from package.json, surfaced to the app as the global
// `__APP_RELEASE__` (used by the client-error telemetry reporter).
const pkg = JSON.parse(
  readFileSync(fileURLToPath(new URL("./package.json", import.meta.url)), "utf-8"),
) as { version: string };

// Dev server proxies /api and /ws to the Rust backend so the SPA is same-origin
// (the backend denies cross-origin by default). Production: the backend serves
// the built bundle from `dist/`, so requests are already same-origin.
export default defineConfig({
  // React Compiler (optimisation audit, §5.2). @vitejs/plugin-react v6 is oxc-based,
  // so the compiler runs via @rolldown/plugin-babel + reactCompilerPreset (not a
  // `babel` plugin option). It reduces re-render *cost* (auto-memoisation); the rAF
  // token batching in Chat.tsx still governs frequency. `include` restricts the
  // Babel pass to component files — unfiltered it parsed every first-party .ts
  // module and dominated build time (re-audit R13).
  plugins: [
    react(),
    babel({ include: [/\.[jt]sx$/], presets: [reactCompilerPreset()] }),
    tailwindcss(),
  ],
  define: { __APP_RELEASE__: JSON.stringify(pkg.version) },
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
      // Edition overlay (open-core split): the Enterprise build points `EDITION_ENTRY`
      // at the sibling private repo's entry (e.g. `../../fosnie-enterprise/frontend/index.tsx`,
      // resolved against this frontend dir) to bundle the Enterprise registrations. The
      // Core build leaves it unset and resolves to an empty stub — no Enterprise UI/routes
      // are bundled or registered, and Core references no Enterprise file.
      "@edition": process.env.EDITION_ENTRY
        ? resolve(process.cwd(), process.env.EDITION_ENTRY)
        : fileURLToPath(new URL("./src/ext/edition-empty.ts", import.meta.url)),
      // The Enterprise entry lives outside this project root (sibling repo) and has no
      // node_modules of its own, so node resolution can't find its bare deps. Pin them to
      // Core's node_modules (these are Core's own runtime deps — edition-neutral). Only
      // applied for the Enterprise build so the Core resolution path is untouched.
      ...(process.env.EDITION_ENTRY
        ? {
            react: fileURLToPath(new URL("./node_modules/react", import.meta.url)),
            "react-dom": fileURLToPath(new URL("./node_modules/react-dom", import.meta.url)),
            "@tanstack/react-query": fileURLToPath(new URL("./node_modules/@tanstack/react-query", import.meta.url)),
          }
        : {}),
    },
  },
  server: {
    port: 5173,
    proxy: {
      "/api": { target: "http://localhost:8080", changeOrigin: true },
      "/health": { target: "http://localhost:8080", changeOrigin: true },
      "/ws": { target: "ws://localhost:8080", ws: true, changeOrigin: true },
    },
  },
  build: { outDir: "dist" },
});
