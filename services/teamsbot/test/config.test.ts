import { describe, expect, it } from 'bun:test';
import { loadConfig } from '../src/config.js';

describe('loadConfig', () => {
  it('parses quoted false boolean env values as false', () => {
    const config = loadConfig({
      TEAMS_DOWNLOAD_ATTACHMENTS: 'false',
      TEAMS_REQUIRE_MENTION: 'false',
    });

    expect(config.teams.attachmentDownloadEnabled).toBe(false);
    expect(config.teams.requireMention).toBe(false);
  });

  it('parses numeric boolean env values explicitly', () => {
    const config = loadConfig({
      TEAMS_DOWNLOAD_ATTACHMENTS: '1',
      TEAMS_REQUIRE_MENTION: '0',
    });

    expect(config.teams.attachmentDownloadEnabled).toBe(true);
    expect(config.teams.requireMention).toBe(false);
  });

  it('prefers the Teamsbot API key over the shared Centaur API key', () => {
    const config = loadConfig({
      CENTAUR_API_KEY: 'shared-key',
      TEAMSBOT_API_KEY: 'teams-key',
    });

    expect(config.centaur.apiKey).toBe('teams-key');
  });
});
