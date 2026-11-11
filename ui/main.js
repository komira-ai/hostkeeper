// The browser's entry: starts the app with the page's document, fetch, confirm and clock.
import { startApp } from "./app.js";

// Offers `file` to the operator as a download: `{name, mime, text}` or `{name, mime, base64}`.
function download(file) {
  const bytes = "base64" in file
    ? Uint8Array.from(window.atob(file.base64), (c) => c.charCodeAt(0))
    : new TextEncoder().encode(file.text);
  const url = URL.createObjectURL(new Blob([bytes], { type: file.mime }));
  const link = document.createElement("a");
  link.setAttribute("href", url);
  link.setAttribute("download", file["name"]);
  document.body.appendChild(link);
  link.click();
  link.remove();
  URL.revokeObjectURL(url);
}

startApp({
  doc: document,
  root: document.getElementById("app"),
  fetch: (url, init) => window.fetch(url, init),
  confirm: (question) => window.confirm(question),
  now: () => Math.floor(Date.now() / 1000),
  download,
});
