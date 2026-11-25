// Identities shared by the fake Slack server and the e2e test driver.
export const TEAM = { id: 'TE2E00001', name: 'Centaur E2E', domain: 'centaur-e2e' }
/** A partner workspace whose members reach the bot through Slack Connect; allowlisted in e2e/infra/values.yaml. */
export const EXTERNAL_TEAM = { id: 'TE2EEXT01', name: 'Partner E2E', domain: 'partner-e2e' }
export const BOT = { id: 'BE2E00001', userId: 'UE2EBOT01', name: 'centaur', appId: 'AE2E00001' }

export type SlackUser = {
  id: string
  name: string
  displayName: string
  email: string
  teamId: string
  /** GitHub handle in the user's Slack profile, if they set one. */
  github?: string
  token: string
}

export const USER: SlackUser = {
  id: 'UE2EUSER1',
  name: 'e2e-user',
  displayName: 'E2E User',
  email: 'e2e-user@example.com',
  teamId: TEAM.id,
  github: 'e2e-user-gh',
  token: 'xoxp-centaur-e2e-user'
}
export const USER_B: SlackUser = {
  id: 'UE2EUSER2',
  name: 'e2e-builder',
  displayName: 'E2E Builder',
  email: 'e2e-builder@example.com',
  teamId: TEAM.id,
  token: 'xoxp-centaur-e2e-user-b'
}
export const EXTERNAL_USER: SlackUser = {
  id: 'UE2EEXT01',
  name: 'partner',
  displayName: 'Partner Person',
  email: 'partner@partner.example',
  teamId: EXTERNAL_TEAM.id,
  token: 'xoxp-centaur-e2e-external'
}
export const USERS = [USER, USER_B, EXTERNAL_USER]

export const CHANNEL = { id: 'CE2E00001', name: 'centaur-e2e' }
/** A channel with slackbotv2 channel defaults, set in e2e/infra/values.yaml. */
export const DEFAULTS_CHANNEL = { id: 'CE2E00002', name: 'centaur-e2e-defaults' }
export const CHANNELS = [CHANNEL, DEFAULTS_CHANNEL]
