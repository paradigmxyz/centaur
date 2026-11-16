/// <reference types="vitest/config" />
import fs from "node:fs";
import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, type Plugin } from "vite";

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

// Rails serves public/ without a CSP, so the built index.html must not live
// there: move it to spa-shell/, where only SpaController serves it (with the CSP).
const SHELL_DIR = path.resolve(import.meta.dirname, "../spa-shell");

function moveShellOutOfPublic(): Plugin {
  let outDir = "";
  return {
    name: "console-shell-out-of-public",
    apply: "build",
    configResolved(config) {
      outDir = path.resolve(config.root, config.build.outDir);
    },
    closeBundle() {
      const built = path.join(outDir, "index.html");
      if (!fs.existsSync(built)) return;
      fs.mkdirSync(SHELL_DIR, { recursive: true });
      fs.copyFileSync(built, path.join(SHELL_DIR, "index.html"));
      fs.rmSync(built);
    },
  };
}

export default defineConfig(({ command }) => ({
  base: command === "build" ? "/spa/" : "/",
  plugins: [react(), tailwindcss(), themeBootScript(), moveShellOutOfPublic()],
  resolve: {
    alias: { "@": path.resolve(import.meta.dirname, "src") },
  },
  // Favicons and PWA icons; Rails serves them in production.
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
