// Applies the stored theme before first paint, so the page never flashes the
// wrong theme. index.html loads it as a classic, render-blocking script ahead
// of the module bundle; vite.config.ts emits it as a content-hashed file under
// /spa/assets/ so the Content-Security-Policy needs no inline script. Keep the
// keys and logic in step with src/lib/theme.ts (same keys as the legacy
// console).
(() => {
  let theme = null;
  try {
    const stored = localStorage.getItem("centaur-console-theme");
    if (localStorage.getItem("centaur-console-theme-source") === "manual" && (stored === "light" || stored === "dark")) theme = stored;
  } catch (_error) {}
  if (!theme) theme = window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
  document.documentElement.dataset.consoleTheme = theme;
  document.documentElement.classList.toggle("dark", theme === "dark");
  const favicon = document.querySelector("[data-console-favicon]");
  if (favicon) favicon.setAttribute("href", theme === "light" ? "/icon-light.svg" : "/icon-dark.svg");
})();
