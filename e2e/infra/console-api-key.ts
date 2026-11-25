// Prints a new Console API key for the e2e run, minted as an operator would:
// sign in with the bootstrap admin's password, then create a key on the
// Console's API Keys endpoint.
//
//   CONSOLE_URL=... ADMIN_EMAIL=... ADMIN_PASSWORD=... bun e2e/infra/console-api-key.ts
function required(name: string): string {
  const value = process.env[name]
  if (!value) throw new Error(`${name} is required`)
  return value
}

const consoleUrl = required('CONSOLE_URL')
const login = await fetch(`${consoleUrl}/login`, {
  method: 'POST',
  redirect: 'manual',
  headers: { origin: consoleUrl, 'content-type': 'application/x-www-form-urlencoded' },
  body: new URLSearchParams({ email: required('ADMIN_EMAIL'), password: required('ADMIN_PASSWORD') })
})
const cookie = login.headers
  .getSetCookie()
  .map(header => header.split(';')[0])
  .join('; ')
if (login.status !== 302 || !cookie) throw new Error(`Console sign-in failed: ${login.status}`)

const created = await fetch(`${consoleUrl}/api/v1/console/api_keys`, {
  method: 'POST',
  headers: { cookie, origin: consoleUrl, 'content-type': 'application/json' },
  body: JSON.stringify({ data: { name: `e2e-${new Date().toISOString()}` } })
})
if (created.status !== 201) throw new Error(`creating the e2e API key failed: ${created.status}`)
const { data } = (await created.json()) as { data: { token: string } }
process.stdout.write(data.token)
