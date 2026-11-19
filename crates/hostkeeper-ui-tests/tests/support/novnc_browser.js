// What a browser gives the vendored noVNC (`ui/vendor/novnc/`) as it loads and runs, for a test
// that runs its `RFB` in the engine: a document whose elements keep their children, style and
// listeners; canvases whose 2D context keeps real RGBA pixels (`putImageData`, `getImageData`,
// `drawImage` of another canvas, `fillRect` with an `rgb(...)` or `#rrggbb` style), so a test reads
// back what was drawn; `ImageData`; a `ResizeObserver` and a `MutationObserver` that observe nothing; `Event` and `CustomEvent`; and timers and
// animation frames that never run by themselves: each is queued, and the test runs the queue
// (`runTimers()`). Imported ahead of the library. Any other method of an element or a context is a
// no-op that returns nothing.

const quiet = { debug() {}, info() {}, warn() {}, error() {}, log() {} };

// `String.prototype.substr`, which every browser has and this engine has not (noVNC reads the
// server's version with it): `length` characters from `start`, a negative start counted from the end.
if (typeof String.prototype.substr !== "function") {
  Object.defineProperty(String.prototype, "substr", {
    value(start, length) {
      const size = this.length;
      const from = start < 0 ? Math.max(size + start, 0) : Math.min(start, size);
      const count = length === undefined ? size - from : Math.max(Math.min(length, size - from), 0);
      return this.slice(from, from + count);
    },
    configurable: true,
    writable: true,
  });
}

// The global `escape`, which every browser has and this engine has not (noVNC decodes the
// desktop's name with it): each character outside `A-Za-z0-9@*_+-./` as `%XX`, or `%uXXXX` past
// 255, as the standard writes it.
if (typeof globalThis.escape !== "function") {
  globalThis.escape = (text) => Array.from(String(text), (ch) => {
    if (/[A-Za-z0-9@*_+\-./]/.test(ch)) {
      return ch;
    }
    const code = ch.charCodeAt(0);
    const hex = code.toString(16).toUpperCase();
    return code < 256 ? `%${hex.padStart(2, "0")}` : `%u${hex.padStart(4, "0")}`;
  }).join("");
}

// Timers and animation frames, in the order asked; the test runs them.
const queued = [];
export function runTimers() {
  let ran = 0;
  while (queued.length > 0) {
    const next = queued.shift();
    next();
    ran += 1;
  }
  return ran;
}
function setTimeout(run, ms, ...args) {
  queued.push(() => run(...args));
  return queued.length;
}
function clearTimeout() {}

// A context of `canvas`, with real pixels of the canvas's size (RGBA, 0 where nothing was drawn).
class Context2D {
  constructor(canvas) {
    this.canvas = canvas;
    this.fillStyle = "#000000";
  }
  _fit() {
    const { width, height } = this.canvas;
    const pixels = this.canvas.pixels;
    if (!pixels || pixels.width !== width || pixels.height !== height) {
      const fresh = new Uint8ClampedArray(width * height * 4);
      this.canvas.pixels = { width, height, data: fresh };
    }
    return this.canvas.pixels;
  }
  getImageData(x, y, w, h) {
    const px = this._fit();
    const out = new ImageData(new Uint8ClampedArray(w * h * 4), w, h);
    for (let row = 0; row < h; row++) {
      for (let col = 0; col < w; col++) {
        const sx = x + col;
        const sy = y + row;
        if (sx < 0 || sy < 0 || sx >= px.width || sy >= px.height) {
          continue;
        }
        for (let c = 0; c < 4; c++) {
          out["data"][(row * w + col) * 4 + c] = px["data"][(sy * px.width + sx) * 4 + c];
        }
      }
    }
    return out;
  }
  putImageData(img, x, y) {
    const px = this._fit();
    for (let row = 0; row < img.height; row++) {
      for (let col = 0; col < img.width; col++) {
        const dx = x + col;
        const dy = y + row;
        if (dx < 0 || dy < 0 || dx >= px.width || dy >= px.height) {
          continue;
        }
        for (let c = 0; c < 4; c++) {
          px["data"][(dy * px.width + dx) * 4 + c] = img["data"][(row * img.width + col) * 4 + c];
        }
      }
    }
  }
  // `drawImage(source, sx, sy, sw, sh, dx, dy, dw, dh)` of another canvas, unscaled.
  drawImage(source, sx, sy, sw, sh, dx, dy) {
    if (!source || typeof source.getContext !== "function") {
      return;
    }
    const img = source.getContext("2d").getImageData(sx, sy, sw, sh);
    this.putImageData(img, dx, dy);
  }
  fillRect(x, y, w, h) {
    const rgb = /^rgb\((\d+),\s*(\d+),\s*(\d+)\)$/.exec(this.fillStyle);
    const hex = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(this.fillStyle);
    const parts = rgb ? rgb.slice(1).map(Number) : hex ? hex.slice(1).map((p) => parseInt(p, 16)) : [0, 0, 0];
    const img = new ImageData(new Uint8ClampedArray(w * h * 4), w, h);
    for (let i = 0; i < w * h; i++) {
      img["data"].set([parts[0], parts[1], parts[2], 255], i * 4);
    }
    this.putImageData(img, x, y);
  }
}

// Any method an object of the fake does not have is a no-op.
function lenient(target) {
  return new Proxy(target, {
    get(obj, key) {
      if (key in obj || typeof key === "symbol") {
        return obj[key];
      }
      return () => undefined;
    },
  });
}

class Element {
  constructor(tag) {
    this.tagName = tag.toUpperCase();
    this.children = [];
    this.parentNode = null;
    this["style"] = {};
    this.listeners = {};
    this.classList = { add() {}, remove() {}, contains: () => false, toggle() {} };
    this.offsetWidth = 0;
    this.offsetHeight = 0;
    this.clientWidth = 640;
    this.clientHeight = 480;
    this.width = 0;
    this.height = 0;
    this.tabIndex = 0;
    this.context = null;
  }
  appendChild(child) {
    child.parentNode = this;
    this.children.push(child);
    return child;
  }
  removeChild(child) {
    const at = this.children.indexOf(child);
    if (at < 0) {
      const error = new Error("not a child");
      error["name"] = "NotFoundError";
      throw error;
    }
    this.children.splice(at, 1);
    child.parentNode = null;
    return child;
  }
  contains(other) {
    return other === this || this.children.some((c) => c.contains(other));
  }
  addEventListener(type, run) {
    (this.listeners[type] = this.listeners[type] || []).push(run);
  }
  removeEventListener() {}
  setAttribute() {}
  getAttribute() {
    return null;
  }
  focus() {}
  blur() {}
  getBoundingClientRect() {
    return { left: 0, top: 0, right: this.clientWidth, bottom: this.clientHeight, width: this.clientWidth, height: this.clientHeight, x: 0, y: 0 };
  }
  getContext(kind) {
    if (this.tagName !== "CANVAS" || kind !== "2d") {
      return null;
    }
    if (this.context === null) {
      this.context = lenient(new Context2D(this));
    }
    return this.context;
  }
}

class ImageData {
  constructor(data, width, height) {
    this["data"] = data;
    this.width = width;
    this.height = height;
  }
}

class ResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}
class MutationObserver extends ResizeObserver {}

// The events noVNC dispatches to its listeners (`connect`, `credentialsrequired`, ...).
class Event {
  constructor(type, init) {
    this.type = type;
    this.bubbles = Boolean(init && init.bubbles);
    this.defaultPrevented = false;
  }
  preventDefault() {
    this.defaultPrevented = true;
  }
  stopPropagation() {}
}
class CustomEvent extends Event {
  constructor(type, init) {
    super(type, init);
    this.detail = init && "detail" in init ? init.detail : null;
  }
}

const body = lenient(new Element("body"));
const documentElement = lenient(new Element("html"));
documentElement.appendChild(body);
const document = lenient({
  body,
  documentElement,
  createElement: (tag) => lenient(new Element(tag)),
  getElementById: () => null,
  elementFromPoint: () => null,
  contains: (el) => documentElement.contains(el),
  addEventListener() {},
  removeEventListener() {},
});

const window = lenient({
  isSecureContext: true,
  devicePixelRatio: 1,
  console: quiet,
  Error,
  document,
  navigator: { userAgent: "test", platform: "test", maxTouchPoints: 0 },
  requestAnimationFrame: (run) => setTimeout(run, 0),
  setTimeout,
  clearTimeout,
  getComputedStyle: () => ({ cursor: "auto" }),
  addEventListener() {},
  removeEventListener() {},
  crypto: { getRandomValues: (array) => array },
});

globalThis.window = window;
globalThis.document = document;
globalThis.navigator = window.navigator;
globalThis.console = quiet;
globalThis.setTimeout = setTimeout;
globalThis.clearTimeout = clearTimeout;
globalThis.ImageData = ImageData;
globalThis.ResizeObserver = ResizeObserver;
globalThis.MutationObserver = MutationObserver;
globalThis.Event = Event;
globalThis.CustomEvent = CustomEvent;
globalThis.WebSocket = { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 };
