import { defineConfig } from "vitest/config";
import { fileURLToPath, URL } from "node:url";

// Unit tests only: no JSX, no DOM rendering, no plugins — the suite covers the
// request layer's pure logic and the repository guard, both of which run in
// plain Node. Keeping the app's Vite plugin chain out of it makes the run fast
// and independent of the build.
export default defineConfig({
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
