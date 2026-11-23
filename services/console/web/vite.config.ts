/// <reference types="vitest/config" />
import fs from "node:fs";
import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, type Plugin } from "vite";

// api-rs serves the production build: the shell at
// services/console/public/spa/index.html (with the Content-Security-Policy) and
// hashed assets under /spa/assets. The dev server runs at / and proxies the
// server-owned paths below to the Console host of a running api-rs.
const apiUrl = process.env.CONSOLE_API_URL ?? "http://localhost:3000";
const apiPaths = ["/api", "/login", "/logout", "/pending", "/auth", "/oauth", "/mcp", "/.well-known"];

// theme-boot.js must run before first paint as a classic script. The CSP has
// no inline allowance, so emit it as a hashed asset and inject a <script src>.
const THEME_BOOT_NAME = "theme-boot.js";
const themeBootPath = path.resolve(import.meta.dirname, THEME_BOOT_NAME);

function themeBootScript(): Plugin {
  let isBuild = false;
  let base = "/";
  return {
    name: "console-theme-boot",
    configResolved(config) {
      isBuild = config.command === "build";
      base = config.base;
    },
    buildStart() {
      if (!isBuild) return;
      this.emitFile({ type: "asset", name: THEME_BOOT_NAME, source: fs.readFileSync(themeBootPath, "utf8") });
    },
    configureServer(server) {
      server.middlewares.use(`/${THEME_BOOT_NAME}`, (_req, res) => {
        res.setHeader("Content-Type", "text/javascript");
        res.end(fs.readFileSync(themeBootPath, "utf8"));
      });
    },
    transformIndexHtml(_html, ctx) {
      let src = `/${THEME_BOOT_NAME}`;
      if (ctx.bundle) {
        const asset = Object.values(ctx.bundle).find(
          (output) => output.type === "asset" && output.names.includes(THEME_BOOT_NAME),
        );
        if (!asset) throw new Error(`${THEME_BOOT_NAME} was not emitted`);
        src = `${base}${asset.fileName}`;
      }
      return [{ tag: "script", attrs: { src }, injectTo: "head" }];
    },
  };
}

export default defineConfig(({ command }) => ({
  base: command === "build" ? "/spa/" : "/",
  plugins: [react(), tailwindcss(), themeBootScript()],
  resolve: {
    alias: { "@": path.resolve(import.meta.dirname, "src") },
  },
  // Favicons and the server-rendered page assets; api-rs serves them in production.
  publicDir: command === "build" ? false : "../public",
  server: {
    port: 5173,
    proxy: Object.fromEntries(
      // Keep the Host header: api-rs routes the Console by host, and its
      // CENTAUR_CONSOLE_PUBLIC_URL must be this dev server's origin.
      apiPaths.map((p) => [p, { target: apiUrl, changeOrigin: false }]),
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
