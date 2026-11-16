// The browser's entry: starts the app with the page's document, fetch, confirm and clock.
import { startApp } from "./app.js";

// The theme the operator chose is shown through the page's root element; the app keeps it in
// memory only (browser storage is not used anywhere: the scan of the sources forbids it).
const prefs = {
  apply: (theme) => {
    if (theme === "system") {
      document.documentElement.removeAttribute("data-theme");
    } else {
      document.documentElement.setAttribute("data-theme", theme);
    }
  },
};

startApp({
  doc: document,
  root: document.getElementById("app"),
  fetch: (url, init) => window.fetch(url, init),
  confirm: (question) => window.confirm(question),
  now: () => Math.floor(Date.now() / 1000),
  prefs,
  every: (run, milliseconds) => {
    const timer = window.setInterval(run, milliseconds);
    return () => window.clearInterval(timer);
  },
});
