import { z } from 'zod';
import { normalizeGoogleChatbotLogLevel, type GoogleChatbotLogLevel } from './logger.js';

const envSchema = z.object({
  PORT: z.coerce.number().int().min(1).max(65535).default(3101),
  LOG_LEVEL: z.string().default('info'),
  GOOGLECHATBOT_DATABASE_URL: z.string().optional(),
  DATABASE_URL: z.string().optional(),
  POSTGRES_URL: z.string().optional(),
  GOOGLECHATBOT_STATE_KEY_PREFIX: z.string().optional(),
  CENTAUR_API_URL: z.string().url().default('http://127.0.0.1:8080'),
  GOOGLECHATBOT_API_KEY: z.string().optional(),
  CENTAUR_REQUEST_MAX_RETRIES: z.coerce.number().int().min(0).default(2),
  CENTAUR_REQUEST_RETRY_DELAY_MS: z.coerce.number().int().min(0).default(250),
  // Google Chat app identity and auth.
  GOOGLE_CHAT_CREDENTIALS: z.string().optional(),
  GOOGLE_CHAT_USE_ADC: envBoolean(false),
  GOOGLE_CHAT_BOT_USER_ID: z.string().optional(),
  GOOGLE_CHAT_USER_NAME: z.string().default('centaur'),
  GOOGLE_CHAT_IMPERSONATE_USER: z.string().optional(),
  // Inbound request verification. At least one of these must be set; the
  // adapter refuses to construct otherwise.
  GOOGLE_CHAT_PROJECT_NUMBER: z.string().optional(),
  GOOGLE_CHAT_ENDPOINT_URL: z.string().optional(),
  GOOGLE_CHAT_WORKSPACE_ADDON_SERVICE_ACCOUNT_EMAIL: z.string().optional(),
  // Workspace Events / Pub/Sub delivery, used to receive every message in a
  // space instead of only @mentions.
  GOOGLE_CHAT_PUBSUB_TOPIC: z.string().optional(),
  GOOGLE_CHAT_PUBSUB_AUDIENCE: z.string().optional(),
  GOOGLE_CHAT_PUBSUB_SERVICE_ACCOUNT_EMAIL: z.string().optional(),
  // Policy.
  GCHAT_ALLOWED_SPACE_IDS: z.string().default(''),
  GCHAT_ALLOWED_DOMAINS: z.string().default(''),
  GCHAT_ALLOWED_SENDER_EMAILS: z.string().default(''),
  GCHAT_ALLOW_DIRECT_MESSAGES: envBoolean(false),
  GCHAT_REQUIRE_MENTION: envBoolean(true),
  GCHAT_DEFAULT_HARNESS_TYPE: z.string().default('codex'),
  GCHAT_ACTIVE_EXECUTION_TTL_MS: z.coerce.number().int().positive().default(30 * 60 * 1000),
  GCHAT_RENDER_DELIVERY_TIMEOUT_MS: z.coerce.number().int().positive().default(15_000),
  GCHAT_RENDER_MIN_EDIT_INTERVAL_MS: z.coerce.number().int().min(0).default(1_500),
  SESSION_IDLE_TIMEOUT_MS: z.coerce.number().int().positive().optional(),
  SESSION_MAX_DURATION_MS: z.coerce.number().int().positive().optional(),
  GCHAT_IDLE_TIMEOUT_MS: z.coerce.number().int().positive().optional(),
  GCHAT_MAX_DURATION_MS: z.coerce.number().int().positive().optional(),
  GCHAT_DOWNLOAD_ATTACHMENTS: envBoolean(false),
  GCHAT_ATTACHMENT_MAX_BYTES: z.coerce.number().int().positive().default(10 * 1024 * 1024),
});

export type GoogleChatServiceAccountCredentials = {
  client_email: string;
  private_key: string;
  project_id?: string;
};

export type GoogleChatbotConfig = {
  centaur: {
    apiKey?: string;
    apiUrl: string;
    requestMaxRetries: number;
    requestRetryDelayMs: number;
  };
  gchat: {
    activeExecutionTtlMs: number;
    allowDirectMessages: boolean;
    allowedDomains: string[];
    allowedSenderEmails: string[];
    allowedSpaceIds: string[];
    attachmentDownloadEnabled: boolean;
    attachmentMaxBytes: number;
    botUserId?: string;
    credentials?: GoogleChatServiceAccountCredentials;
    defaultHarnessType: string;
    endpointUrl?: string;
    idleTimeoutMs?: number;
    impersonateUser?: string;
    maxDurationMs?: number;
    projectNumber?: string;
    pubsubAudience?: string;
    pubsubServiceAccountEmail?: string;
    pubsubTopic?: string;
    renderDeliveryTimeoutMs: number;
    renderMinEditIntervalMs: number;
    requireMention: boolean;
    useApplicationDefaultCredentials: boolean;
    userName: string;
    workspaceAddOnServiceAccountEmail?: string;
  };
  server: {
    logLevel: GoogleChatbotLogLevel;
    port: number;
    postgresUrl?: string;
    stateKeyPrefix?: string;
  };
};

export function loadConfig(env: NodeJS.ProcessEnv = process.env): GoogleChatbotConfig {
  const parsed = envSchema.parse(env);
  return {
    centaur: {
      apiKey: parsed.GOOGLECHATBOT_API_KEY,
      apiUrl: parsed.CENTAUR_API_URL,
      requestMaxRetries: parsed.CENTAUR_REQUEST_MAX_RETRIES,
      requestRetryDelayMs: parsed.CENTAUR_REQUEST_RETRY_DELAY_MS,
    },
    gchat: {
      activeExecutionTtlMs: parsed.GCHAT_ACTIVE_EXECUTION_TTL_MS,
      allowDirectMessages: parsed.GCHAT_ALLOW_DIRECT_MESSAGES,
      allowedDomains: parseCsv(parsed.GCHAT_ALLOWED_DOMAINS).map((domain) => domain.toLowerCase()),
      allowedSenderEmails: parseCsv(parsed.GCHAT_ALLOWED_SENDER_EMAILS).map((email) => email.toLowerCase()),
      allowedSpaceIds: parseCsv(parsed.GCHAT_ALLOWED_SPACE_IDS).map(normalizeSpaceId),
      attachmentDownloadEnabled: parsed.GCHAT_DOWNLOAD_ATTACHMENTS,
      attachmentMaxBytes: parsed.GCHAT_ATTACHMENT_MAX_BYTES,
      botUserId: parsed.GOOGLE_CHAT_BOT_USER_ID,
      credentials: parseServiceAccountCredentials(parsed.GOOGLE_CHAT_CREDENTIALS),
      defaultHarnessType: parsed.GCHAT_DEFAULT_HARNESS_TYPE,
      endpointUrl: parsed.GOOGLE_CHAT_ENDPOINT_URL,
      idleTimeoutMs: parsed.GCHAT_IDLE_TIMEOUT_MS ?? parsed.SESSION_IDLE_TIMEOUT_MS,
      impersonateUser: parsed.GOOGLE_CHAT_IMPERSONATE_USER,
      maxDurationMs: parsed.GCHAT_MAX_DURATION_MS ?? parsed.SESSION_MAX_DURATION_MS,
      projectNumber: parsed.GOOGLE_CHAT_PROJECT_NUMBER,
      pubsubAudience: parsed.GOOGLE_CHAT_PUBSUB_AUDIENCE,
      pubsubServiceAccountEmail: parsed.GOOGLE_CHAT_PUBSUB_SERVICE_ACCOUNT_EMAIL,
      pubsubTopic: parsed.GOOGLE_CHAT_PUBSUB_TOPIC,
      renderDeliveryTimeoutMs: parsed.GCHAT_RENDER_DELIVERY_TIMEOUT_MS,
      renderMinEditIntervalMs: parsed.GCHAT_RENDER_MIN_EDIT_INTERVAL_MS,
      requireMention: parsed.GCHAT_REQUIRE_MENTION,
      useApplicationDefaultCredentials: parsed.GOOGLE_CHAT_USE_ADC,
      userName: parsed.GOOGLE_CHAT_USER_NAME,
      workspaceAddOnServiceAccountEmail: parsed.GOOGLE_CHAT_WORKSPACE_ADDON_SERVICE_ACCOUNT_EMAIL,
    },
    server: {
      logLevel: normalizeGoogleChatbotLogLevel(parsed.LOG_LEVEL),
      port: parsed.PORT,
      postgresUrl: parsed.GOOGLECHATBOT_DATABASE_URL ?? parsed.DATABASE_URL ?? parsed.POSTGRES_URL,
      stateKeyPrefix: parsed.GOOGLECHATBOT_STATE_KEY_PREFIX,
    },
  };
}

/**
 * A Chat app authenticates as a service account (JSON key) or through
 * Application Default Credentials. Without either, every Chat API call — the
 * whole reply path — fails at runtime, so refuse to boot instead.
 */
export function assertGoogleChatCredentials(config: GoogleChatbotConfig): void {
  if (config.gchat.useApplicationDefaultCredentials || config.gchat.credentials) {
    return;
  }
  throw new Error(
    'GOOGLE_CHAT_CREDENTIALS (service account JSON) or GOOGLE_CHAT_USE_ADC=true is required',
  );
}

/**
 * Inbound verification is fail-closed: the adapter only accepts a Google-signed
 * webhook when it knows which audience to expect. Catch a half-configured app
 * here rather than at the first delivery.
 */
export function assertGoogleChatWebhookVerification(config: GoogleChatbotConfig): void {
  if (config.gchat.projectNumber || config.gchat.endpointUrl) {
    return;
  }
  throw new Error(
    'GOOGLE_CHAT_PROJECT_NUMBER or GOOGLE_CHAT_ENDPOINT_URL is required to verify inbound Google Chat webhooks',
  );
}

/**
 * Pub/Sub push is only trustworthy when both the audience and the pushing
 * service account are pinned: the audience is a public URL anyone can have
 * Google mint a token for.
 */
export function assertGoogleChatPubsubVerification(config: GoogleChatbotConfig): void {
  if (!config.gchat.pubsubTopic && !config.gchat.pubsubAudience) {
    return;
  }
  if (config.gchat.pubsubAudience && config.gchat.pubsubServiceAccountEmail) {
    return;
  }
  throw new Error(
    'GOOGLE_CHAT_PUBSUB_AUDIENCE and GOOGLE_CHAT_PUBSUB_SERVICE_ACCOUNT_EMAIL are both required when Pub/Sub delivery is configured',
  );
}

function parseServiceAccountCredentials(value: string | undefined): GoogleChatServiceAccountCredentials | undefined {
  const raw = value?.trim();
  if (!raw) {
    return undefined;
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new Error('GOOGLE_CHAT_CREDENTIALS must be service account JSON');
  }
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    throw new Error('GOOGLE_CHAT_CREDENTIALS must be service account JSON');
  }
  const candidate = parsed as Record<string, unknown>;
  const clientEmail = candidate.client_email;
  const privateKey = candidate.private_key;
  if (typeof clientEmail !== 'string' || !clientEmail.trim()) {
    throw new Error('GOOGLE_CHAT_CREDENTIALS is missing client_email');
  }
  if (typeof privateKey !== 'string' || !privateKey.trim()) {
    throw new Error('GOOGLE_CHAT_CREDENTIALS is missing private_key');
  }
  return {
    client_email: clientEmail,
    // Secret stores frequently hold the key with escaped newlines; the Google
    // auth client needs the real ones.
    private_key: privateKey.includes('\\n') ? privateKey.replaceAll('\\n', '\n') : privateKey,
    ...(typeof candidate.project_id === 'string' ? { project_id: candidate.project_id } : {}),
  };
}

/** Accept both `spaces/AAAA` resource names and bare `AAAA` space ids. */
function normalizeSpaceId(value: string): string {
  const trimmed = value.trim();
  return trimmed.startsWith('spaces/') ? trimmed : `spaces/${trimmed}`;
}

function parseCsv(value: string): string[] {
  return value
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean);
}

function envBoolean(defaultValue: boolean): z.ZodType<boolean> {
  return z.preprocess((value) => {
    if (value === undefined) {
      return defaultValue;
    }
    if (typeof value === 'string') {
      switch (value.trim().toLowerCase()) {
        case '1':
        case 'true':
          return true;
        case '0':
        case 'false':
          return false;
      }
    }
    return value;
  }, z.boolean());
}
