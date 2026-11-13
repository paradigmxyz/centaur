// Runs once per cell, before the cell. Captures the host bindings in
// closures and removes them from globalThis, so cells reach the network and
// files only through connectors and `files`.
(() => {
  const host = globalThis.__host;
  const spec = globalThis.__spec;
  delete globalThis.__host;
  delete globalThis.__spec;

  const TOOL_TIMEOUT_MS = spec.tool_timeout_ms;

  class TextEncoder {
    get encoding() { return "utf-8"; }
    encode(text = "") { return host.utf8Encode(String(text)); }
  }
  class TextDecoder {
    get encoding() { return "utf-8"; }
    decode(bytes) {
      if (bytes === undefined) return "";
      return host.utf8Decode(toBytes(bytes));
    }
  }
  globalThis.TextEncoder = TextEncoder;
  globalThis.TextDecoder = TextDecoder;

  function toBytes(value) {
    if (value instanceof Uint8Array) return value;
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    if (typeof value === "string") return host.utf8Encode(value);
    throw new TypeError("expected a string, ArrayBuffer or Uint8Array");
  }

  let nextTimer = 1;
  const cancelled = new Set();
  globalThis.setTimeout = (callback, ms = 0, ...args) => {
    const id = nextTimer++;
    host.sleep(Math.max(0, Number(ms) || 0)).then(() => {
      if (cancelled.delete(id)) return;
      callback(...args);
    });
    return id;
  };
  globalThis.clearTimeout = (id) => { cancelled.add(id); };
  const sleep = (ms) => host.sleep(Math.max(0, Number(ms) || 0));

  const format = (value) => {
    if (typeof value === "string") return value;
    try { return JSON.stringify(value); } catch { return String(value); }
  };
  const log = (level) => (...values) => host.log(level, values.map(format).join(" "));
  globalThis.console = Object.freeze({
    log: log("log"), info: log("info"), warn: log("warn"), error: log("error"), debug: log("debug"),
  });

  class Response {
    #body;
    constructor(status, headers, body) {
      this.status = status;
      this.ok = status >= 200 && status < 300;
      this.headers = headers;
      this.#body = body;
    }
    async bytes() { return new Uint8Array(this.#body); }
    async text() { return host.utf8Decode(new Uint8Array(this.#body)); }
    async json() { return JSON.parse(await this.text()); }
  }

  async function fetch(url, init = {}) {
    const method = String(init.method ?? "GET").toUpperCase();
    const headers = {};
    for (const [key, value] of Object.entries(init.headers ?? {})) {
      if (value !== undefined && value !== null) headers[key] = String(value);
    }
    const body = init.body === undefined || init.body === null ? undefined : toBytes(init.body);
    const raw = body === undefined
      ? await host.fetch(String(url), method, JSON.stringify(headers))
      : await host.fetch(String(url), method, JSON.stringify(headers), body);
    return new Response(raw.status, JSON.parse(raw.headers), raw.body);
  }

  const files = Object.freeze({
    read: async (path) => new Uint8Array(await host.fileRead(String(path))),
    readText: async (path) => host.utf8Decode(new Uint8Array(await host.fileRead(String(path)))),
    write: async (path, data) => {
      const bytes = await host.fileWrite(String(path), toBytes(data));
      return { path: String(path), bytes };
    },
  });

  const principal = Object.freeze({
    ...spec.principal,
    oauth: Object.freeze([...(spec.principal.oauth ?? [])]),
  });
  const ctx = Object.freeze({
    fetch,
    secret: (name) => host.secret(String(name)) ?? undefined,
    principal,
    files,
    sleep,
  });

  const calls = [];
  globalThis.__calls = calls;

  const withTimeout = (promise, ms, label) => {
    let id;
    const timeout = new Promise((_, reject) => {
      id = setTimeout(() => reject(new Error(`${label} timed out after ${ms / 1000}s`)), ms);
    });
    return Promise.race([promise, timeout]).finally(() => clearTimeout(id));
  };

  const tools = Object.create(null);
  for (const [name, functions] of Object.entries(spec.connectors)) {
    let module;
    const namespace = Object.create(null);
    for (const fn of functions) {
      const tool = `${name}.${fn}`;
      namespace[fn] = async (args = {}) => {
        const started = Date.now();
        try {
          module ??= import(`connector:${name}`);
          const exports = await module;
          const result = await withTimeout(Promise.resolve(exports[fn](args, ctx)), TOOL_TIMEOUT_MS, tool);
          calls.push({ tool, ms: Date.now() - started, ok: true });
          return result;
        } catch (error) {
          const message = error instanceof Error ? error.message : format(error);
          calls.push({ tool, ms: Date.now() - started, ok: false, error: message });
          const wrapped = error instanceof Error ? error : new Error(message);
          if (wrapped.tool === undefined) {
            try { wrapped.tool = tool; } catch {}
          }
          throw wrapped;
        }
      };
    }
    tools[name] = Object.freeze(namespace);
  }
  globalThis.tools = Object.freeze(tools);
  globalThis.files = files;
  globalThis.sleep = sleep;
})();
