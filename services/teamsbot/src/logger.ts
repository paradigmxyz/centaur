import type { Logger } from 'chat';

export type TeamsbotLogLevel = 'debug' | 'info' | 'warn' | 'error' | 'silent';

const LOG_LEVELS: readonly TeamsbotLogLevel[] = ['debug', 'info', 'warn', 'error', 'silent'];

export function normalizeTeamsbotLogLevel(value: string | undefined): TeamsbotLogLevel {
  const normalized = value?.toLowerCase();
  return LOG_LEVELS.includes(normalized as TeamsbotLogLevel)
    ? normalized as TeamsbotLogLevel
    : 'info';
}

export function createTeamsbotLogger(minLevel: TeamsbotLogLevel): Logger {
  const logger: Logger = {
    debug: (message: string, data?: unknown) => log(minLevel, 'debug', message, data),
    info: (message: string, data?: unknown) => log(minLevel, 'info', message, data),
    warn: (message: string, data?: unknown) => log(minLevel, 'warn', message, data),
    error: (message: string, data?: unknown) => log(minLevel, 'error', message, data),
    child: () => logger,
  };
  return logger;
}

function log(minLevel: TeamsbotLogLevel, level: Exclude<TeamsbotLogLevel, 'silent'>, event: string, data?: unknown): void {
  if (minLevel === 'silent' || LOG_LEVELS.indexOf(level) < LOG_LEVELS.indexOf(minLevel)) {
    return;
  }
  console.log(JSON.stringify({
    level,
    service: 'teamsbot',
    timestamp: new Date().toISOString(),
    event,
    ...logFields(data),
  }));
}

function logFields(data: unknown): Record<string, unknown> {
  if (!data || typeof data !== 'object' || Array.isArray(data)) {
    return {};
  }
  return Object.fromEntries(
    Object.entries(data).map(([key, value]) => [key, value instanceof Error ? errorFields(value) : value]),
  );
}

function errorFields(error: Error): Record<string, unknown> {
  return {
    message: error.message,
    name: error.name,
    stack: error.stack,
  };
}
