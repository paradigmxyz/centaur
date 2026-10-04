// Identities shared by the fake Slack server and the e2e test driver.
export const TEAM = { id: 'TE2E00001', name: 'Centaur E2E', domain: 'centaur-e2e' }
export const BOT = { id: 'BE2E00001', userId: 'UE2EBOT01', name: 'centaur', appId: 'AE2E00001' }
export const USER = { id: 'UE2EUSER1', name: 'e2e-user', email: 'e2e-user@example.com' }
export const CHANNEL = { id: 'CE2E00001', name: 'centaur-e2e' }
/** A channel with slackbotv2 channel defaults, set in e2e/infra/values.yaml. */
export const DEFAULTS_CHANNEL = { id: 'CE2E00002', name: 'centaur-e2e-defaults' }
export const CHANNELS = [CHANNEL, DEFAULTS_CHANNEL]
export const USER_TOKEN = 'xoxp-centaur-e2e-user'
