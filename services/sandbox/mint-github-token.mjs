// Mint a GitHub App installation token from GITHUB_APP_ID +
// GITHUB_APP_PRIVATE_KEY (PEM) in the environment. Self-contained (node:crypto
// only); runs under bun at sandbox boot (entrypoint.sh) so agents get a
// short-lived GITHUB_TOKEN instead of any long-lived credential.
//
// Installation selection: prefer the account whose login matches
// GITHUB_APP_INSTALLATION_OWNER (default "darkmatter"); deprioritize
// enterprise-typed installations — an empty enterprise install with no login
// once shadowed the org install and every clone failed "Repository not found"
// (effect-github-app 20ed762).
import { createSign } from "node:crypto";

const appId = process.env.GITHUB_APP_ID;
const pem = process.env.GITHUB_APP_PRIVATE_KEY;
if (!appId || !pem) {
  console.error("mint-github-token: GITHUB_APP_ID / GITHUB_APP_PRIVATE_KEY unset");
  process.exit(2);
}

const b64u = (buf) => Buffer.from(buf).toString("base64url");
const now = Math.floor(Date.now() / 1000);
const header = b64u(JSON.stringify({ alg: "RS256", typ: "JWT" }));
const payload = b64u(JSON.stringify({ iat: now - 60, exp: now + 540, iss: appId }));
const signer = createSign("RSA-SHA256");
signer.update(`${header}.${payload}`);
const jwt = `${header}.${payload}.${signer.sign(pem, "base64url")}`;

const gh = (path, init = {}) =>
  fetch(`https://api.github.com${path}`, {
    ...init,
    headers: {
      accept: "application/vnd.github+json",
      "user-agent": "centaur-sandbox-token-mint",
      authorization: `Bearer ${jwt}`,
      ...init.headers,
    },
  });

const owner = (process.env.GITHUB_APP_INSTALLATION_OWNER ?? "darkmatter").toLowerCase();
const resp = await gh("/app/installations?per_page=100");
if (!resp.ok) {
  console.error(`mint-github-token: installations ${resp.status}: ${(await resp.text()).slice(0, 200)}`);
  process.exit(1);
}
const installations = await resp.json();
const score = (i) =>
  (i.account?.login?.toLowerCase() === owner ? 2 : 0) +
  (i.target_type !== "Enterprise" && i.account?.type !== "Enterprise" ? 1 : 0);
const pick = installations.sort((a, b) => score(b) - score(a))[0];
if (!pick) {
  console.error("mint-github-token: no installations for this App");
  process.exit(1);
}

const tok = await gh(`/app/installations/${pick.id}/access_tokens`, { method: "POST" });
if (!tok.ok) {
  console.error(`mint-github-token: access_tokens ${tok.status}: ${(await tok.text()).slice(0, 200)}`);
  process.exit(1);
}
const { token } = await tok.json();
console.log(token);
