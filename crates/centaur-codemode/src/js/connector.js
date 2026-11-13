// Connector realm, fresh for every call. Evaluates to the `ctx` passed to the
// connector function. `host.fetch` is bound by the host to this call.
(() => {
  const { host, toBytes, sleep, context } = globalThis.__common;
  const env = Object.freeze({ ...globalThis.__env });
  delete globalThis.__common;
  delete globalThis.__env;

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

  // A request body: bytes, or parts (strings, bytes, `{ file }`) the host
  // joins, so file contents never pass through this realm.
  const isFilePart = (value) => value !== null && typeof value === "object" && typeof value.file === "string";
  const bodyPart = (value) => (isFilePart(value) ? { file: value.file } : toBytes(value));
  const requestBody = (body) => {
    if (body === undefined || body === null) return undefined;
    if (Array.isArray(body)) return body.map(bodyPart);
    return bodyPart(body);
  };
  const requestHeaders = (init) => {
    const headers = {};
    for (const [key, value] of Object.entries(init.headers ?? {})) {
      if (value !== undefined && value !== null) headers[key] = String(value);
    }
    return JSON.stringify(headers);
  };

  const fetch = async (url, init = {}) => {
    const method = String(init.method ?? "GET").toUpperCase();
    const body = requestBody(init.body);
    const raw = body === undefined
      ? await host.fetch(String(url), method, requestHeaders(init))
      : await host.fetch(String(url), method, requestHeaders(init), body);
    return new Response(raw.status, JSON.parse(raw.headers), raw.body);
  };

  // The files capability; `null` unless the runner was started with --files-dir.
  const files = typeof host.filesRead !== "function" ? null : Object.freeze({
    read: async (path) => (await host.filesRead(String(path))).data,
    readText: async (path) => host.utf8Decode((await host.filesRead(String(path))).data),
    write: (path, data) => host.filesWrite(String(path), toBytes(data)),
    stat: (path) => host.filesStat(String(path)),
    download: async (url, path, init = {}) => {
      const method = String(init.method ?? "GET").toUpperCase();
      const body = requestBody(init.body);
      const raw = body === undefined
        ? await host.filesDownload(String(url), String(path), method, requestHeaders(init))
        : await host.filesDownload(String(url), String(path), method, requestHeaders(init), body);
      const ok = raw.status >= 200 && raw.status < 300;
      return Object.freeze({ ...raw, ok, headers: JSON.parse(raw.headers) });
    },
  });

  const base64 = Object.freeze({
    encode: (data, options = {}) => host.base64Encode(toBytes(data), Boolean(options.url)),
    decode: (text) => host.base64Decode(String(text)),
    decodeText: (text) => host.utf8Decode(host.base64Decode(String(text))),
  });

  return Object.freeze({
    fetch,
    env,
    context,
    sleep,
    base64,
    files,
    gzip: (data) => host.gzip(toBytes(data)),
    gunzip: (data) => host.gunzip(toBytes(data)),
  });
})();
