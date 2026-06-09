import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { TanStackRouterVite } from "@tanstack/router-plugin/vite";
import electron from "vite-plugin-electron/simple";
import { fileURLToPath } from "node:url";
import path from "node:path";

const rootDir = path.dirname(fileURLToPath(import.meta.url));

// Electron build/dev is opt-in via `ELECTRON=1` (see the electron:* scripts) so
// the plain web build stays untouched.
const useElectron = !!process.env.ELECTRON;

// SPA only — no SSR. This is a pure client-side WebGL + File System Access app.
export default defineConfig({
  // Relative asset paths so index.html loads correctly from file:// in packaged Electron.
  base: useElectron ? "./" : "/",
  plugins: [
    TanStackRouterVite({ target: "react" }),
    react(),
    ...(useElectron
      ? [
          // Emit CommonJS .cjs for both: in a CJS Electron main, require("electron")
          // resolves to the built-in API (named ESM imports hit the npm wrapper
          // instead). .cjs also loads regardless of this package's "type":"module".
          electron({
            main: {
              entry: "electron/main.ts",
              vite: {
                build: {
                  // Bypass the plugin's lib config (which forces ESM for
                  // "type":"module") and drive a CJS bundle via rollupOptions.
                  lib: false,
                  rollupOptions: {
                    input: "electron/main.ts",
                    output: {
                      format: "cjs",
                      entryFileNames: "[name].cjs",
                      inlineDynamicImports: true,
                    },
                  },
                },
              },
            },
            preload: {
              input: "electron/preload.ts",
              vite: {
                build: {
                  rollupOptions: {
                    output: { format: "cjs", entryFileNames: "[name].cjs" },
                  },
                },
              },
            },
          }),
        ]
      : []),
  ],
  resolve: {
    alias: {
      "@": path.resolve(rootDir, "./src"),
    },
  },
  build: {
    // Three.js is inherently ~600 kB; split vendors into their own cached chunks.
    chunkSizeWarningLimit: 900,
    rollupOptions: {
      output: {
        manualChunks: {
          three: ["three"],
          react: ["react", "react-dom"],
        },
      },
    },
  },
});
