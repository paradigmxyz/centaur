// Prints SQL that stores subscription access tokens in the console's
// broker_credentials table as token-broker would after a refresh, encrypted
// with the release's Active Record encryption keys. e2e/stack.sh pipes it into
// the release Postgres (the console has no REPL to seed them through).
//
//   CENTAUR_CONSOLE_AR_ENCRYPTION_PRIMARY_KEY=... \
//   CENTAUR_CONSOLE_AR_ENCRYPTION_KEY_DERIVATION_SALT=... \
//   bun e2e/lib/broker-credentials-sql.ts openai-codex=TOKEN anthropic-claude=TOKEN
//
// Without a refresh token token-broker never tries to refresh them.
import { createCipheriv, pbkdf2Sync, randomBytes } from 'node:crypto'

function required(name: string): string {
  const value = process.env[name]
  if (!value) throw new Error(`${name} is not set`)
  return value
}

// ActiveRecord::Encryption's non-deterministic scheme: AES-256-GCM under a key
// derived from the primary key with PBKDF2-SHA256 (2^16 iterations), stored as
// {"p":payload,"h":{"iv":...,"at":...}}. Tokens here are short, so Rails would
// not compress them (it compresses above 140 bytes).
const key = pbkdf2Sync(
  required('CENTAUR_CONSOLE_AR_ENCRYPTION_PRIMARY_KEY'),
  required('CENTAUR_CONSOLE_AR_ENCRYPTION_KEY_DERIVATION_SALT'),
  1 << 16,
  32,
  'sha256'
)

function encrypt(plaintext: string): string {
  if (Buffer.byteLength(plaintext) > 140) throw new Error('token too long for an uncompressed envelope')
  const iv = randomBytes(12)
  const cipher = createCipheriv('aes-256-gcm', key, iv)
  const payload = Buffer.concat([cipher.update(plaintext, 'utf8'), cipher.final()])
  return JSON.stringify({ p: payload.toString('base64'), h: { iv: iv.toString('base64'), at: cipher.getAuthTag().toString('base64') } })
}

const literal = (value: string) => `'${value.replaceAll("'", "''")}'`

const statements = ['BEGIN;']
for (const argument of process.argv.slice(2)) {
  const index = argument.indexOf('=')
  if (index <= 0) throw new Error(`expected foreign_id=token, got ${argument}`)
  const foreignId = literal(argument.slice(0, index))
  const token = literal(encrypt(argument.slice(index + 1)))
  statements.push(
    `UPDATE broker_credentials SET access_token = ${token}, last_refresh = now() AT TIME ZONE 'UTC',
       expires_at = (now() + interval '1 year') AT TIME ZONE 'UTC', updated_at = now() AT TIME ZONE 'UTC'
     WHERE foreign_id = ${foreignId};`,
    `INSERT INTO broker_credentials (foreign_id, token_endpoint, client_id, access_token, last_refresh, expires_at, created_at, updated_at)
     SELECT ${foreignId}, 'https://oauth.invalid/token', 'centaur-e2e', ${token}, now() AT TIME ZONE 'UTC',
       (now() + interval '1 year') AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC', now() AT TIME ZONE 'UTC'
     WHERE NOT EXISTS (SELECT 1 FROM broker_credentials WHERE foreign_id = ${foreignId});`
  )
}
statements.push('COMMIT;')
console.log(statements.join('\n'))
