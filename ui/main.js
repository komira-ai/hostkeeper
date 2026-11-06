// The browser's entry: starts the app with the page's document, fetch, confirm and clock.
import { startApp } from "./app.js";

startApp({
  doc: document,
  root: document.getElementById("app"),
  fetch: (url, init) => window.fetch(url, init),
  confirm: (question) => window.confirm(question),
  now: () => Math.floor(Date.now() / 1000),
});
