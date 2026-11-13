// Cell realm. `tools.<connector>.<fn>(args)` sends JSON to the host, which
// checks policy, records the call and runs the connector in its own realm.
// A cell that replaces anything here only changes what it sees itself.
(() => {
  const { host, sleep, files } = globalThis.__common;
  const connectors = globalThis.__connectors;
  delete globalThis.__common;
  delete globalThis.__connectors;

  const tools = Object.create(null);
  for (const [name, functions] of Object.entries(connectors)) {
    const namespace = Object.create(null);
    for (const fn of functions) {
      namespace[fn] = async (args = {}) => {
        const json = JSON.stringify(args);
        if (json === undefined) throw new TypeError(`${name}.${fn} args must be JSON-serializable`);
        return JSON.parse(await host.invoke(`${name}.${fn}`, json));
      };
    }
    tools[name] = Object.freeze(namespace);
  }
  globalThis.tools = Object.freeze(tools);
  globalThis.files = files;
  globalThis.sleep = sleep;
})();
