// The browser's entry: starts the app with the page's document, fetch and confirm.
import { startApp } from "./app.js";

startApp({
  doc: document,
  root: document.getElementById("app"),
  fetch: (url, init) => window.fetch(url, init),
  confirm: (question) => window.confirm(question),
});
