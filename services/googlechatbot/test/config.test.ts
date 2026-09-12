import { describe, expect, it } from 'bun:test';
import {
  assertGoogleChatCredentials,
  assertGoogleChatPubsubVerification,
  assertGoogleChatWebhookVerification,
  loadConfig,
} from '../src/config.js';

const SERVICE_ACCOUNT_JSON = JSON.stringify({
  client_email: 'centaur@example.iam.gserviceaccount.com',
  private_key: '-----BEGIN PRIVATE KEY-----\\nabc\\n-----END PRIVATE KEY-----\\n',
  project_id: 'centaur-demo',
});

describe('loadConfig', () => {
  it('parses allowlists and normalizes space ids to resource names', () => {
    const config = loadConfig({
      GCHAT_ALLOWED_SPACE_IDS: 'spaces/AAAA, BBBB ',
      GCHAT_ALLOWED_DOMAINS: 'Example.com, other.test',
      GCHAT_ALLOWED_SENDER_EMAILS: 'Casey@Example.com',
      GOOGLE_CHAT_USE_ADC: 'true',
    } as NodeJS.ProcessEnv);

    expect(config.gchat.allowedSpaceIds).toEqual(['spaces/AAAA', 'spaces/BBBB']);
    expect(config.gchat.allowedDomains).toEqual(['example.com', 'other.test']);
    expect(config.gchat.allowedSenderEmails).toEqual(['casey@example.com']);
  });

  it('defaults to mention-gated, DM-off, attachment-download-off', () => {
    const config = loadConfig({ GOOGLE_CHAT_USE_ADC: 'true' } as NodeJS.ProcessEnv);

    expect(config.gchat.requireMention).toBe(true);
    expect(config.gchat.allowDirectMessages).toBe(false);
    expect(config.gchat.attachmentDownloadEnabled).toBe(false);
    expect(config.gchat.allowedSpaceIds).toEqual([]);
  });

  it('unescapes newlines in a service account private key', () => {
    const config = loadConfig({ GOOGLE_CHAT_CREDENTIALS: SERVICE_ACCOUNT_JSON } as NodeJS.ProcessEnv);

    expect(config.gchat.credentials?.private_key).toContain('\n');
    expect(config.gchat.credentials?.private_key).not.toContain('\\n');
    expect(config.gchat.credentials?.client_email).toBe('centaur@example.iam.gserviceaccount.com');
  });

  it('rejects credentials that are not service account JSON', () => {
    expect(() => loadConfig({ GOOGLE_CHAT_CREDENTIALS: 'not-json' } as NodeJS.ProcessEnv))
      .toThrow('GOOGLE_CHAT_CREDENTIALS must be service account JSON');
    expect(() => loadConfig({ GOOGLE_CHAT_CREDENTIALS: '{"client_email":"a@b.c"}' } as NodeJS.ProcessEnv))
      .toThrow('GOOGLE_CHAT_CREDENTIALS is missing private_key');
  });
});

describe('startup assertions', () => {
  it('requires a credential source', () => {
    const config = loadConfig({ GOOGLE_CHAT_PROJECT_NUMBER: '1' } as NodeJS.ProcessEnv);
    expect(() => assertGoogleChatCredentials(config)).toThrow('GOOGLE_CHAT_CREDENTIALS');
    expect(() => assertGoogleChatCredentials(loadConfig({ GOOGLE_CHAT_USE_ADC: 'true' } as NodeJS.ProcessEnv))).not.toThrow();
  });

  it('requires a webhook verification audience', () => {
    expect(() => assertGoogleChatWebhookVerification(loadConfig({ GOOGLE_CHAT_USE_ADC: 'true' } as NodeJS.ProcessEnv)))
      .toThrow('GOOGLE_CHAT_PROJECT_NUMBER or GOOGLE_CHAT_ENDPOINT_URL');
    expect(() => assertGoogleChatWebhookVerification(loadConfig({
      GOOGLE_CHAT_USE_ADC: 'true',
      GOOGLE_CHAT_ENDPOINT_URL: 'https://chat.example/api/webhooks/gchat',
    } as NodeJS.ProcessEnv))).not.toThrow();
  });

  it('requires both Pub/Sub audience and pushing identity when Pub/Sub is configured', () => {
    expect(() => assertGoogleChatPubsubVerification(loadConfig({
      GOOGLE_CHAT_USE_ADC: 'true',
      GOOGLE_CHAT_PUBSUB_TOPIC: 'projects/demo/topics/chat',
    } as NodeJS.ProcessEnv))).toThrow('GOOGLE_CHAT_PUBSUB_AUDIENCE');
    expect(() => assertGoogleChatPubsubVerification(loadConfig({
      GOOGLE_CHAT_USE_ADC: 'true',
      GOOGLE_CHAT_PUBSUB_TOPIC: 'projects/demo/topics/chat',
      GOOGLE_CHAT_PUBSUB_AUDIENCE: 'https://chat.example/api/webhooks/gchat/pubsub',
      GOOGLE_CHAT_PUBSUB_SERVICE_ACCOUNT_EMAIL: 'push@demo.iam.gserviceaccount.com',
    } as NodeJS.ProcessEnv))).not.toThrow();
    // No Pub/Sub configured at all is fine: @mention webhooks still work.
    expect(() => assertGoogleChatPubsubVerification(loadConfig({ GOOGLE_CHAT_USE_ADC: 'true' } as NodeJS.ProcessEnv)))
      .not.toThrow();
  });
});
