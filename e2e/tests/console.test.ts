// The console worker syncs the Slack channel catalog from the bot token in
// its environment; the scheduled-task channel picker and destination checks
// read that catalog.
import { expect, test } from 'bun:test'
import { consoleSession, eventually } from '../lib'

test('the console channel picker lists the Slack channels the worker synced', async () => {
  const page = await eventually('the console Slack channel catalog', 60_000, async () => {
    const response = await consoleSession.ok('GET', '/api/v1/console/slack_channels?q=centaur-e2e')
    return response.meta.catalog_error === null ? response : undefined
  })
  expect(page.data.map((channel: { channel_id: string }) => channel.channel_id))
    .toEqual(expect.arrayContaining(['CE2E00001', 'CE2E00002']))
}, 90_000)
