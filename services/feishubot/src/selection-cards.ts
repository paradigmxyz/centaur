const MAX_FALLBACK = 500
const MAX_QUERY = 100
const MAX_SELECTED = 50

export type RepositorySummary = {
  repository_id: string
  display_name: string
  path_with_namespace: string
  default_branch: string
  archived: boolean
  description?: string | null
}

export type SelectionCardState = {
  selectionFlowId: string
  expectedVersion: number
  taskExcerpt: string
  query: string
  cursor: string | null
  cursorHistory: string[]
  nextCursor: string | null
  selectedRepositoryIds: string[]
  repositories: RepositorySummary[]
  status: 'pending' | 'confirmed' | 'cancelled'
}

export type SelectionAction =
  | { action: 'toggle'; expectedVersion: number; repositoryId: string }
  | { action: 'remove'; expectedVersion: number; repositoryId: string }
  | { action: 'search'; expectedVersion: number; query: string }
  | { action: 'next'; expectedVersion: number }
  | { action: 'previous'; expectedVersion: number }

export type RenderedCard = {
  fallbackText: string
  card: Record<string, unknown>
}

export function applySelectionAction(
  state: SelectionCardState,
  action: SelectionAction
): SelectionCardState {
  if (state.status !== 'pending' || action.expectedVersion !== state.expectedVersion) {
    throw new Error('selection card is stale')
  }
  switch (action.action) {
    case 'toggle': {
      validateRepositoryId(action.repositoryId)
      const selected = new Set(state.selectedRepositoryIds)
      if (selected.has(action.repositoryId)) selected.delete(action.repositoryId)
      else {
        if (selected.size >= MAX_SELECTED) throw new Error('too many selected repositories')
        selected.add(action.repositoryId)
      }
      return { ...state, selectedRepositoryIds: [...selected] }
    }
    case 'remove':
      return {
        ...state,
        selectedRepositoryIds: state.selectedRepositoryIds.filter(id => id !== action.repositoryId)
      }
    case 'search':
      if ([...action.query].length > MAX_QUERY) throw new Error('repository query is too long')
      return {
        ...state,
        query: action.query.trim(),
        cursor: null,
        cursorHistory: [],
        nextCursor: null,
        repositories: []
      }
    case 'next':
      if (!state.nextCursor) return state
      return {
        ...state,
        cursorHistory: [...state.cursorHistory, state.cursor ?? ''],
        cursor: state.nextCursor,
        nextCursor: null,
        repositories: []
      }
    case 'previous': {
      const history = state.cursorHistory.slice()
      const cursor = history.pop()
      return {
        ...state,
        cursor: cursor || null,
        cursorHistory: history,
        nextCursor: null,
        repositories: []
      }
    }
  }
}

export function renderSelectionCard(state: SelectionCardState): RenderedCard {
  const pending = state.status === 'pending'
  const value = (action: string, extra: Record<string, unknown> = {}) => ({
    action,
    selection_flow_id: state.selectionFlowId,
    expected_version: state.expectedVersion,
    ...extra
  })
  const repositoryElements = state.repositories.slice(0, 20).map(repository => ({
    tag: 'div',
    text: {
      tag: 'lark_md',
      content: `**${plain(repository.display_name, 80)}**\n${plain(repository.path_with_namespace, 120)}`
    },
    extra: {
      tag: 'button',
      type: state.selectedRepositoryIds.includes(repository.repository_id) ? 'primary' : 'default',
      text: { tag: 'plain_text', content: state.selectedRepositoryIds.includes(repository.repository_id) ? '已选择' : '选择' },
      value: value('toggle', { repository_id: repository.repository_id }),
      disabled: !pending
    }
  }))
  const selected = state.selectedRepositoryIds.slice(0, MAX_SELECTED).map(repositoryId => ({
    tag: 'button',
    type: 'default',
    text: { tag: 'plain_text', content: `移除 ${repositoryId}` },
    value: value('remove', { repository_id: repositoryId }),
    disabled: !pending
  }))
  const elements: unknown[] = [
    { tag: 'markdown', content: plain(state.taskExcerpt, 500) },
    {
      tag: 'form',
      name: 'repository_search',
      elements: [
        {
          tag: 'input',
          name: 'query',
          label: { tag: 'plain_text', content: '搜索项目' },
          placeholder: { tag: 'plain_text', content: '项目名称或路径' },
          default_value: state.query,
          disabled: !pending
        },
        {
          tag: 'button',
          action_type: 'form_submit',
          text: { tag: 'plain_text', content: '搜索' },
          value: value('search'),
          disabled: !pending
        }
      ]
    },
    ...repositoryElements,
    ...(selected.length ? [{ tag: 'action', actions: selected }] : []),
    {
      tag: 'action',
      actions: [
        { tag: 'button', text: { tag: 'plain_text', content: '上一页' }, value: value('previous'), disabled: !pending || state.cursorHistory.length === 0 },
        { tag: 'button', text: { tag: 'plain_text', content: '下一页' }, value: value('next'), disabled: !pending || !state.nextCursor },
        { tag: 'button', type: 'primary', text: { tag: 'plain_text', content: '确认' }, value: value('confirm'), disabled: !pending },
        { tag: 'button', text: { tag: 'plain_text', content: '暂不选择项目' }, value: value('no_project'), disabled: !pending },
        { tag: 'button', text: { tag: 'plain_text', content: '取消' }, value: value('cancel'), disabled: !pending }
      ]
    }
  ]
  return {
    fallbackText: bounded(`选择项目：已选择 ${state.selectedRepositoryIds.length} 个项目`),
    card: {
      schema: '2.0',
      config: { update_multi: true },
      header: { title: { tag: 'plain_text', content: '选择项目' }, template: 'blue' },
      body: { elements }
    }
  }
}

type PublishItem = {
  repository_id: string
  state: string
  merge_request_url?: string | null
  failure_message?: string | null
}

export function renderPublicationCard(batch: {
  publish_batch_id: string
  changeset_id: string
  state: string
  items: PublishItem[]
}): RenderedCard {
  const succeeded = batch.items.filter(item => item.state === 'succeeded')
  const failed = batch.items.filter(item => item.state === 'failed')
  const title = batch.state === 'partially_succeeded'
    ? '发布部分成功'
    : batch.state === 'succeeded' ? '发布完成' : batch.state === 'failed' ? '发布失败' : '正在发布'
  const elements: unknown[] = [
    ...succeeded.map(item => ({
      tag: 'markdown',
      content: item.merge_request_url
        ? `${item.repository_id}：[查看 MR](${item.merge_request_url})`
        : `${item.repository_id}：已完成`
    })),
    ...failed.map(item => ({ tag: 'markdown', content: `${item.repository_id}：发布失败` }))
  ]
  if (failed.length > 0 && ['failed', 'partially_succeeded'].includes(batch.state)) {
    elements.push({
      tag: 'button',
      type: 'primary',
      text: { tag: 'plain_text', content: '重试失败项目' },
      value: { action: 'retry_failed', publish_batch_id: batch.publish_batch_id }
    })
  }
  return {
    fallbackText: bounded(`${title}：成功 ${succeeded.length}，失败 ${failed.length}`),
    card: {
      schema: '2.0',
      config: { update_multi: true },
      header: { title: { tag: 'plain_text', content: title }, template: failed.length ? 'orange' : 'green' },
      body: { elements }
    }
  }
}

export function renderProgressCard(input: {
  title?: string
  status?: string
  answer?: string
  changesetId?: string
  consoleUrl?: string
  failed?: boolean
}): RenderedCard {
  const title = input.title || (input.failed ? '任务失败' : input.changesetId ? '代码修改已完成' : '正在处理')
  const elements: unknown[] = []
  if (input.status) elements.push({ tag: 'markdown', content: plain(input.status, 500) })
  if (input.answer) elements.push({ tag: 'markdown', content: plain(input.answer, 2_500) })
  if (input.changesetId) {
    elements.push({ tag: 'markdown', content: `ChangeSet：${plain(input.changesetId, 100)}` })
    if (input.consoleUrl) {
      elements.push({
        tag: 'button',
        type: 'default',
        text: { tag: 'plain_text', content: '查看修改' },
        url: input.consoleUrl
      })
    }
    elements.push({
      tag: 'button',
      type: 'primary',
      text: { tag: 'plain_text', content: '创建 MR' },
      value: { action: 'approve_publication', changeset_id: input.changesetId }
    })
  }
  if (elements.length === 0) {
    elements.push({ tag: 'markdown', content: input.failed ? '任务未完成' : '任务已提交' })
  }
  return {
    fallbackText: bounded(`${title}${input.status ? `：${input.status}` : ''}`),
    card: {
      schema: '2.0',
      config: { update_multi: true },
      header: {
        title: { tag: 'plain_text', content: title },
        template: input.failed ? 'red' : input.changesetId ? 'green' : 'blue'
      },
      body: { elements }
    }
  }
}

function validateRepositoryId(value: string): void {
  if (!/^gitlab:[1-9][0-9]*$/u.test(value)) throw new Error('invalid repository ID')
}

function plain(value: string, max: number): string {
  return [...value.replace(/[\r\n]+/gu, ' ').trim()].slice(0, max).join('')
}

function bounded(value: string): string {
  return [...value].slice(0, MAX_FALLBACK).join('')
}
