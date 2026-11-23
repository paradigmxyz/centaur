// Prints SQL that stores the subscription access tokens as token-broker would
// after a refresh, for e2e/stack.sh to run in the Console database. The tokens
// are encrypted as Active Record `encrypts` attributes (see
// crates/active-record-encryption): AES-256-GCM under a PBKDF2-SHA256 key.
//
//   E2E_AR_PRIMARY_KEY=... E2E_AR_SALT=... CODEX_ACCESS_TOKEN=... \
//     CLAUDE_ACCESS_TOKEN=... bun e2e/infra/broker-credentials-sql.ts
import { createCipheriv, pbkdf2Sync, randomBytes } from 'node:crypto'

function required(name: string): string {
  const value = process.env[name]
  if (!value) throw new Error(`${name} is required`)
  return value
}

const key = pbkdf2Sync(required('E2E_AR_PRIMARY_KEY'), required('E2E_AR_SALT'), 1 << 16, 32, 'sha256')

function encrypt(plaintext: string): string {
  const iv = randomBytes(12)
  const cipher = createCipheriv('aes-256-gcm', key, iv)
  const ciphertext = Buffer.concat([cipher.update(plaintext, 'utf8'), cipher.final()])
  return JSON.stringify({
    p: ciphertext.toString('base64'),
    h: { iv: iv.toString('base64'), at: cipher.getAuthTag().toString('base64') }
  })
}

const quote = (value: string) => `'${value.replaceAll("'", "''")}'`

const tokens = {
  'openai-codex': required('CODEX_ACCESS_TOKEN'),
  'anthropic-claude': required('CLAUDE_ACCESS_TOKEN')
}
for (const [foreignId, token] of Object.entries(tokens)) {
  console.log(`
INSERT INTO broker_credentials
  (foreign_id, token_endpoint, client_id, access_token, last_refresh, expires_at, created_at, updated_at)
VALUES (${quote(foreignId)}, 'https://oauth.invalid/token', 'centaur-e2e', ${quote(encrypt(token))},
        now() AT TIME ZONE 'UTC', (now() + interval '1 year') AT TIME ZONE 'UTC',
        now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC')
ON CONFLICT (foreign_id) DO UPDATE SET
  token_endpoint = EXCLUDED.token_endpoint, client_id = EXCLUDED.client_id,
  access_token = EXCLUDED.access_token, last_refresh = EXCLUDED.last_refresh,
  expires_at = EXCLUDED.expires_at, updated_at = EXCLUDED.updated_at;`)
}
