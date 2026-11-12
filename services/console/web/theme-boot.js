// Applies the stored theme before first paint. Keep the keys and logic in step
// with src/lib/theme.ts.
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
