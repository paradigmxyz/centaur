// Each supported harness boots and answers a Slack mention through the proxy
// with the credential the stack's auth mode calls for (and, for a ChatGPT
// subscription, the workspace). CI also runs this file alone against a stack
// in access_token mode, the one check of the subscription path.
import { expect, test } from 'bun:test'
import { authMode, chatgptAccountId, model, providerCredentials, slack, turnTimeoutMs } from '../lib'

// Pi supports API keys only (docs/pages/extend/pi-harness.mdx).
const harnesses = authMode === 'access_token' ? ['codex', 'claudecode'] : ['codex', 'claudecode', 'pi']

for (const harness of harnesses) {
  test.concurrent(`${harness} answers a Slack mention`, async () => {
    const answer = model.says('Hello from the harness.')

    const thread = await slack.mention(`--${harness} Say hello.`, answer)
    const turn = await thread.nextTurn()

    expect(turn.reply).toBe(answer.text)
    expect(turn.execution.status).toBe('completed')
    expect(turn.sandbox.harness).toBe(harness)
    const request = turn.request!
    expect(request.credential).toBe(providerCredentials[request.provider])
    expect(request.account).toBe(request.provider === 'openai' ? chatgptAccountId : undefined)
  }, turnTimeoutMs)
}
