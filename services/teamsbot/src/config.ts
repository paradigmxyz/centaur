import { config as loadDotenv } from 'dotenv';
import { z } from 'zod';
import { normalizeTeamsbotLogLevel, type TeamsbotLogLevel } from './logger.js';

loadDotenv({ quiet: true });

const envSchema = z.object({
  PORT: z.coerce.number().int().min(1).max(65535).default(3100),
  LOG_LEVEL: z.string().default('info'),
  TEAMSBOT_DATABASE_URL: z.string().optional(),
  DATABASE_URL: z.string().optional(),
  POSTGRES_URL: z.string().optional(),
  TEAMSBOT_STATE_KEY_PREFIX: z.string().optional(),
  CENTAUR_API_URL: z.string().url().default('http://127.0.0.1:8080'),
  TEAMSBOT_API_KEY: z.string().optional(),
  CENTAUR_API_KEY: z.string().optional(),
  CENTAUR_REQUEST_MAX_RETRIES: z.coerce.number().int().min(0).default(2),
  CENTAUR_REQUEST_RETRY_DELAY_MS: z.coerce.number().int().min(0).default(250),
  TEAMS_BOT_APP_ID: z.string().default(''),
  TEAMS_BOT_APP_PASSWORD: z.string().default(''),
  TEAMS_BOT_APP_TENANT_ID: z.string().default(''),
  TEAMS_ALLOWED_CHANNEL_IDS: z.string().default(''),
  TEAMS_ALLOWED_TEAM_IDS: z.string().default(''),
  TEAMS_ALLOWED_TENANT_IDS: z.string().default(''),
  TEAMS_REQUIRE_MENTION: envBoolean(true),
  TEAMS_DEFAULT_HARNESS_TYPE: z.string().default('codex'),
  TEAMS_ACTIVE_EXECUTION_TTL_MS: z.coerce.number().int().positive().default(30 * 60 * 1000),
  TEAMS_RENDER_DELIVERY_TIMEOUT_MS: z.coerce.number().int().positive().default(15_000),
  SESSION_IDLE_TIMEOUT_MS: z.coerce.number().int().positive().optional(),
  SESSION_MAX_DURATION_MS: z.coerce.number().int().positive().optional(),
  TEAMS_IDLE_TIMEOUT_MS: z.coerce.number().int().positive().optional(),
  TEAMS_MAX_DURATION_MS: z.coerce.number().int().positive().optional(),
  TEAMS_DOWNLOAD_ATTACHMENTS: envBoolean(false),
  TEAMS_ATTACHMENT_MAX_BYTES: z.coerce.number().int().positive().default(10 * 1024 * 1024),
  TEAMS_ATTACHMENT_ALLOWED_HOSTS: z.string().default('graph.microsoft.com,*.sharepoint.com,*.1drv.ms,smba.trafficmanager.net,smba.infra.gov.teams.microsoft.us'),
  TEAMS_GRAPH_BEARER_TOKEN: z.string().optional(),
  TEAMS_GRAPH_TOKEN_SCOPE: z.string().default('https://graph.microsoft.com/.default'),
});

export type TeamsbotConfig = {
  centaur: {
    apiKey?: string;
    apiUrl: string;
    requestMaxRetries: number;
    requestRetryDelayMs: number;
  };
  server: {
    logLevel: TeamsbotLogLevel;
    postgresUrl?: string;
    stateKeyPrefix?: string;
    port: number;
  };
  teams: {
    allowedChannelIds: string[];
    allowedTeamIds: string[];
    allowedTenantIds: string[];
    appId: string;
    appPassword: string;
    appTenantId: string;
    attachmentAllowedHosts: string[];
    attachmentDownloadEnabled: boolean;
    attachmentMaxBytes: number;
    activeExecutionTtlMs: number;
    defaultHarnessType: string;
    graphBearerToken?: string;
    graphTokenScope: string;
    idleTimeoutMs?: number;
    maxDurationMs?: number;
    renderDeliveryTimeoutMs: number;
    requireMention: boolean;
  };
};

export function loadConfig(env: NodeJS.ProcessEnv = process.env): TeamsbotConfig {
  const parsed = envSchema.parse(env);
  return {
    centaur: {
      apiKey: parsed.TEAMSBOT_API_KEY ?? parsed.CENTAUR_API_KEY,
      apiUrl: parsed.CENTAUR_API_URL,
      requestMaxRetries: parsed.CENTAUR_REQUEST_MAX_RETRIES,
      requestRetryDelayMs: parsed.CENTAUR_REQUEST_RETRY_DELAY_MS,
    },
    server: {
      logLevel: normalizeTeamsbotLogLevel(parsed.LOG_LEVEL),
      postgresUrl: parsed.TEAMSBOT_DATABASE_URL ?? parsed.DATABASE_URL ?? parsed.POSTGRES_URL,
      port: parsed.PORT,
      stateKeyPrefix: parsed.TEAMSBOT_STATE_KEY_PREFIX,
    },
    teams: {
      allowedChannelIds: parseCsv(parsed.TEAMS_ALLOWED_CHANNEL_IDS),
      allowedTeamIds: parseCsv(parsed.TEAMS_ALLOWED_TEAM_IDS),
      allowedTenantIds: parseCsv(parsed.TEAMS_ALLOWED_TENANT_IDS),
      appId: parsed.TEAMS_BOT_APP_ID,
      appPassword: parsed.TEAMS_BOT_APP_PASSWORD,
      appTenantId: parsed.TEAMS_BOT_APP_TENANT_ID,
      attachmentAllowedHosts: parseCsv(parsed.TEAMS_ATTACHMENT_ALLOWED_HOSTS),
      attachmentDownloadEnabled: parsed.TEAMS_DOWNLOAD_ATTACHMENTS,
      attachmentMaxBytes: parsed.TEAMS_ATTACHMENT_MAX_BYTES,
      activeExecutionTtlMs: parsed.TEAMS_ACTIVE_EXECUTION_TTL_MS,
      defaultHarnessType: parsed.TEAMS_DEFAULT_HARNESS_TYPE,
      graphBearerToken: parsed.TEAMS_GRAPH_BEARER_TOKEN,
      graphTokenScope: parsed.TEAMS_GRAPH_TOKEN_SCOPE,
      idleTimeoutMs: parsed.TEAMS_IDLE_TIMEOUT_MS ?? parsed.SESSION_IDLE_TIMEOUT_MS,
      maxDurationMs: parsed.TEAMS_MAX_DURATION_MS ?? parsed.SESSION_MAX_DURATION_MS,
      renderDeliveryTimeoutMs: parsed.TEAMS_RENDER_DELIVERY_TIMEOUT_MS,
      requireMention: parsed.TEAMS_REQUIRE_MENTION,
    },
  };
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
