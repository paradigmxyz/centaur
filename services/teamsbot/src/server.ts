import { loadConfig } from './config.js';
import { createTeamsbot } from './index.js';

const config = loadConfig();
const teamsbot = await createTeamsbot({ config });

teamsbot.start();
