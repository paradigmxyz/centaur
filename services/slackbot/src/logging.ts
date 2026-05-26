const EMAIL_RE = /\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b/gi
const SSN_RE = /\b\d{3}-\d{2}-\d{4}\b/g
const PHONE_CANDIDATE_RE = /(?<!\w)(?:\+?\d[\d(). -]{8,}\d)(?!\w)/g
const BEARER_TOKEN_RE = /\bbearer\s+[A-Z0-9._~+/=-]+/gi
const FIELD_SPLIT_RE = /(?<!^)(?=[A-Z])|[^A-Za-z0-9]+/g

const SECRET_FIELD_TOKENS = new Set(['password', 'secret', 'token'])
const SECRET_FIELD_NAMES = new Set([
  'apikey',
  'authorization',
  'clientsecret',
  'accesstoken',
  'refreshtoken'
])
const EMAIL_FIELD_NAMES = new Set(['email', 'useremail', 'authoremail'])
const PHONE_FIELD_NAMES = new Set(['phone', 'phonenumber', 'userphone'])
const SSN_FIELD_NAMES = new Set(['ssn', 'socialsecuritynumber'])
const LOG_VERSION_UUID = '7f3b4a2e-9d7c-4f2a-8b91-3e6d2c0a5f14'

function logVersionFields(): Record<string, string> {
  return {
    log_version_uuid: LOG_VERSION_UUID
  }
}

function normalizeFieldName(fieldName: string | undefined): string {
  return (fieldName ?? '').toLowerCase().replace(/[^a-z0-9]/g, '')
}

function fieldTokens(fieldName: string | undefined): Set<string> {
  return new Set(
    (fieldName ?? '')
      .split(FIELD_SPLIT_RE)
      .filter(Boolean)
      .map(part => part.toLowerCase())
  )
}

function redactPhoneCandidate(candidate: string): string {
  const digits = [...candidate].filter(ch => ch >= '0' && ch <= '9').length
  if (digits >= 10 && digits <= 15 && !candidate.includes(':')) {
    return '[REDACTED:phone]'
  }
  return candidate
}

export function sanitizeLogString(value: string): string {
  return value
    .replace(BEARER_TOKEN_RE, 'Bearer [REDACTED:secret]')
    .replace(EMAIL_RE, '[REDACTED:email]')
    .replace(SSN_RE, '[REDACTED:ssn]')
    .replace(PHONE_CANDIDATE_RE, redactPhoneCandidate)
}

export function sanitizeLogValue(
  value: unknown,
  fieldName?: string,
  seen: WeakSet<object> = new WeakSet()
): unknown {
  if (value === null || value === undefined) return value
  if (typeof value === 'boolean' || typeof value === 'number') return value
  if (typeof value === 'bigint') return value.toString()
  if (typeof value === 'string') {
    const normalizedField = normalizeFieldName(fieldName)
    const tokens = fieldTokens(fieldName)
    if (
      SECRET_FIELD_NAMES.has(normalizedField) ||
      [...tokens].some(token => SECRET_FIELD_TOKENS.has(token))
    ) {
      return '[REDACTED:secret]'
    }
    if (EMAIL_FIELD_NAMES.has(normalizedField) || tokens.has('email')) return '[REDACTED:email]'
    if (PHONE_FIELD_NAMES.has(normalizedField) || tokens.has('phone')) return '[REDACTED:phone]'
    if (SSN_FIELD_NAMES.has(normalizedField) || tokens.has('ssn')) return '[REDACTED:ssn]'
    return sanitizeLogString(value)
  }
  if (typeof value !== 'object') return String(value)
  if (seen.has(value)) return '[Circular]'
  seen.add(value)

  if (value instanceof Error) {
    return {
      name: value.name,
      message: sanitizeLogString(value.message),
      cause: 'cause' in value ? sanitizeLogValue(value.cause, 'cause', seen) : undefined
    }
  }
  if (value instanceof Date) return value.toISOString()
  if (Array.isArray(value)) return value.map(item => sanitizeLogValue(item, fieldName, seen))

  return Object.fromEntries(
    Object.entries(value).map(([key, item]) => [key, sanitizeLogValue(item, key, seen)])
  )
}

function withLogVersionMetadata(values: unknown[]): unknown[] {
  const metadata = logVersionFields()
  if (values.length === 0) return [metadata]
  const [first, ...rest] = values
  if (
    first &&
    typeof first === 'object' &&
    !Array.isArray(first) &&
    !(first instanceof Error) &&
    !(first instanceof Date)
  ) {
    return [{ ...metadata, ...(sanitizeLogValue(first) as Record<string, unknown>) }, ...rest]
  }
  return [metadata, ...values.map(value => sanitizeLogValue(value))]
}

export function logWarn(event: string, ...values: unknown[]): void {
  console.warn(event, ...withLogVersionMetadata(values))
}

export function logInfo(event: string, ...values: unknown[]): void {
  console.log(event, ...withLogVersionMetadata(values))
}

export function logError(event: string, ...values: unknown[]): void {
  console.error(event, ...withLogVersionMetadata(values))
}
