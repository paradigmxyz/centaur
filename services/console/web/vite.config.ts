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

// The pre-paint theme script (theme-boot.js) must run before first paint, so
// it is a classic render-blocking script rather than part of the deferred
// module bundle. Rails sends `script-src 'self'` with no inline allowance
// (config/initializers/content_security_policy.rb), so instead of inlining it
// in index.html this plugin emits it as a content-hashed file under
// /spa/assets/ (long-cacheable, like the bundle) and injects a <script src>
// at the end of <head>. The dev server serves it at /theme-boot.js.
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

// Rails serves public/ through its static file server, which sends no
// Content-Security-Policy. The built shell (index.html) therefore must not live
// there: after the build this plugin moves it to services/console/spa-shell/,
// where only SpaController serves it (with the CSP). public/spa keeps just the
// content-hashed assets.
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
