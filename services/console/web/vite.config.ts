/// <reference types="vitest/config" />
import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Rails serves the production build from services/console/public/spa, so
// built asset URLs live under /spa/. The dev server runs at / and proxies the
// Rails-owned paths below to `bin/rails server`.
const railsUrl = process.env.CONSOLE_RAILS_URL ?? "http://localhost:3000";
const railsPaths = [
  "/api",
  "/login",
  "/logout",
  "/pending",
  "/auth",
  "/oauth",
  "/mcp",
  "/.well-known",
  "/launch",
  "/manifest",
  "/service-worker",
  "/up",
];

export default defineConfig(({ command }) => ({
  base: command === "build" ? "/spa/" : "/",
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { "@": path.resolve(import.meta.dirname, "src") },
  },
  // Rails' public/ holds the favicons and PWA icons. Serve them in dev only;
  // in production Rails serves them itself.
  publicDir: command === "build" ? false : "../public",
  server: {
    port: 5173,
    proxy: Object.fromEntries(
      railsPaths.map((p) => [
        p,
        {
          target: railsUrl,
          changeOrigin: false,
        },
      ]),
    ),
  },
  build: {
    outDir: "../public/spa",
    emptyOutDir: true,
    sourcemap: true,
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    css: false,
  },
}));
