import { describe, expect, it } from 'bun:test'
import {
  applySelectionAction,
  renderPublicationCard,
  renderSelectionCard,
  type SelectionCardState
} from '../src/selection-cards.js'

const initial: SelectionCardState = {
  selectionFlowId: 'sel_1',
  expectedVersion: 2,
  taskExcerpt: '修复支付服务失败的测试',
  query: '',
  cursor: null,
  cursorHistory: [],
  nextCursor: 'next-1',
  selectedRepositoryIds: [],
  repositories: [
    { repository_id: 'gitlab:42', name: 'payments', path_with_namespace: 'backend/payments', default_branch: 'main', archived: false },
    { repository_id: 'gitlab:84', name: 'console', path_with_namespace: 'web/console', default_branch: 'main', archived: false }
  ],
  status: 'pending'
}

describe('Feishu selection cards', () => {
  it('keeps cross-page selections and disables stale cards', () => {
    const selected = applySelectionAction(initial, {
      action: 'toggle', expectedVersion: 2, repositoryId: 'gitlab:42'
    })
    const next = applySelectionAction(selected, {
      action: 'next', expectedVersion: 2
    })
    expect(next.selectedRepositoryIds).toEqual(['gitlab:42'])
    expect(next.cursor).toBe('next-1')
    expect(() => applySelectionAction(next, {
      action: 'toggle', expectedVersion: 1, repositoryId: 'gitlab:84'
    })).toThrow('stale')
  })

  it('renders bounded fallback text and complete mutation actions', () => {
    const card = renderSelectionCard(initial)
    expect(card.fallbackText).toContain('选择项目')
    expect(card.fallbackText.length).toBeLessThanOrEqual(500)
    const actions = JSON.stringify(card.card)
    expect(actions).not.toContain('\"tag\":\"action\"')
    for (const action of ['toggle', 'search', 'next', 'confirm', 'no_project', 'cancel']) {
      expect(actions).toContain(`\"action\":\"${action}\"`)
    }
    expect(actions).not.toContain('clone_url')
  })

  it('names every interactive component in the repository search form', () => {
    const card = renderSelectionCard(initial)
    const body = card.card.body as {
      elements: Array<{ tag: string; elements?: Array<{ name?: string }> }>
    }
    const form = body.elements.find(element => element.tag === 'form')
    if (!form?.elements) throw new Error('repository search form is missing')
    const names = form.elements.map(element => element.name)

    expect(names).toEqual(['query', 'repository_search_submit'])
    expect(new Set(names).size).toBe(names.length)
  })
})

describe('Feishu publication cards', () => {
  it('separates successful MRs from failures and only retries failed items', () => {
    const card = renderPublicationCard({
      publish_batch_id: 'pub_1', changeset_id: 'chg_1', state: 'partially_succeeded',
      items: [
        { repository_id: 'gitlab:42', state: 'succeeded', merge_request_url: 'https://git.example.test/mr/7' },
        { repository_id: 'gitlab:84', state: 'failed', failure_message: 'redacted failure' }
      ]
    })
    expect(card.fallbackText).toContain('部分成功')
    const rendered = JSON.stringify(card.card)
    expect(rendered).toContain('https://git.example.test/mr/7')
    expect(rendered).toContain('\"action\":\"retry_failed\"')
    expect(rendered).not.toContain('redacted failure')
  })
})
