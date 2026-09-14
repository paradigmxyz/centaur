import { loadConfig } from './config.js';
import { createGooglechatbot } from './index.js';

const config = loadConfig();
const googlechatbot = createGooglechatbot({ config });

const server = Bun.serve({ port: config.server.port, fetch: googlechatbot.app.fetch });
googlechatbot.logger.info('googlechatbot_started', { port: server.port });

void googlechatbot.initialize().catch((error) => {
  googlechatbot.logger.error('googlechatbot_startup_initialization_failed', { error });
  process.exitCode = 1;
  setTimeout(() => process.exit(1), 100);
});

const shutdown = async (signal: string): Promise<void> => {
  googlechatbot.logger.info('googlechatbot_shutdown_started', { signal });
  await googlechatbot.chat.shutdown().catch(() => undefined);
  server.stop();
  googlechatbot.logger.info('googlechatbot_shutdown_complete', { signal });
  process.exit(0);
};

process.on('SIGTERM', () => void shutdown('SIGTERM'));
process.on('SIGINT', () => void shutdown('SIGINT'));
