// Runs first in every realm (cells and connector calls). Builds the standard
// globals from the host bindings in `__host`, which the realm-specific prelude
// then consumes. Nothing here is a security boundary: every host function a
// realm receives is safe for code in that realm to call with any arguments.
(() => {
  const host = globalThis.__host;

  const toBytes = (value) => {
    if (value instanceof Uint8Array) return value;
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    if (typeof value === "string") return host.utf8Encode(value);
    throw new TypeError("expected a string, ArrayBuffer or Uint8Array");
  };

  class TextEncoder {
    get encoding() { return "utf-8"; }
    encode(text = "") { return host.utf8Encode(String(text)); }
  }
  class TextDecoder {
    get encoding() { return "utf-8"; }
    decode(bytes) { return bytes === undefined ? "" : host.utf8Decode(toBytes(bytes)); }
  }

  let nextTimer = 1;
  const cancelled = new Set();
  const setTimeout = (callback, ms = 0, ...args) => {
    const id = nextTimer++;
    host.sleep(Math.max(0, Number(ms) || 0)).then(() => {
      if (cancelled.delete(id)) return;
      callback(...args);
    });
    return id;
  };
  const clearTimeout = (id) => { cancelled.add(id); };
  const sleep = (ms) => host.sleep(Math.max(0, Number(ms) || 0));

  const format = (value) => {
    if (typeof value === "string") return value;
    try { return JSON.stringify(value); } catch { return String(value); }
  };
  const log = (level) => (...values) => host.log(level, values.map(format).join(" "));

  // Web-standard Latin-1 base64, for code that expects it.
  const btoa = (text) => {
    const value = String(text);
    const bytes = new Uint8Array(value.length);
    for (let i = 0; i < value.length; i++) {
      const code = value.charCodeAt(i);
      if (code > 255) throw new TypeError("btoa: string contains characters outside Latin-1");
      bytes[i] = code;
    }
    return host.base64Encode(bytes, false);
  };
  const atob = (text) => {
    const bytes = host.base64Decode(String(text));
    let out = "";
    for (let i = 0; i < bytes.length; i += 8192) {
      out += String.fromCharCode(...bytes.subarray(i, i + 8192));
    }
    return out;
  };

  globalThis.btoa = btoa;
  globalThis.atob = atob;
  globalThis.TextEncoder = TextEncoder;
  globalThis.TextDecoder = TextDecoder;
  globalThis.setTimeout = setTimeout;
  globalThis.clearTimeout = clearTimeout;
  globalThis.console = Object.freeze({
    log: log("log"), info: log("info"), warn: log("warn"), error: log("error"), debug: log("debug"),
  });
  // The calling context, read once per cell. Each realm gets its own copy.
  const deepFreeze = (value) => {
    if (value && typeof value === "object") {
      for (const key of Object.keys(value)) deepFreeze(value[key]);
      Object.freeze(value);
    }
    return value;
  };
  const context = deepFreeze(globalThis.__context ?? null);
  delete globalThis.__context;

  globalThis.__common = { host, toBytes, sleep, context };
  delete globalThis.__host;
})();
