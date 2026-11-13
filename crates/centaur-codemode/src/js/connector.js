// Connector realm, fresh for every call. Evaluates to the `ctx` passed to the
// connector function. `host.fetch` is bound by the host to this call.
(() => {
  const { host, toBytes, sleep, files } = globalThis.__common;
  const principal = globalThis.__principal;
  const env = Object.freeze({ ...globalThis.__env });
  delete globalThis.__common;
  delete globalThis.__principal;
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

  const fetch = async (url, init = {}) => {
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
  };

  return Object.freeze({
    fetch,
    env,
    principal: Object.freeze({ ...principal, oauth: Object.freeze([...(principal.oauth ?? [])]) }),
    files,
    sleep,
  });
})();
