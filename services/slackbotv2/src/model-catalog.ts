/** Operator-owned model metadata, carried alongside existing Codex provider config. */
export type CatalogModel = {
  id: string
  provider: string
  providerName: string
  displayName: string
  aliases: string[]
  description?: string
  reasoningEfforts: string[]
  defaultReasoning?: string
  reasoningAliases: Record<string, string>
}

export function configuredModels(): CatalogModel[] {
  const raw = process.env.CODEX_CUSTOM_PROVIDERS
  if (!raw) return []
  try {
    const providers = JSON.parse(raw)
    if (!object(providers)) return []
    return Object.entries(providers).flatMap(([provider, config]) => {
      if (!object(config) || !object(config.models)) return []
      return Object.entries(config.models).flatMap(([id, model]) => {
        if (!object(model)) return []
        const efforts = strings(model.reasoningEfforts)
        const aliases = object(model.reasoningAliases)
          ? Object.fromEntries(Object.entries(model.reasoningAliases).filter(
            (entry): entry is [string, string] => typeof entry[1] === 'string' && efforts.includes(entry[1])
          )) : {}
        return [{
          id, provider, providerName: typeof config.name === 'string' ? config.name : provider,
          displayName: typeof model.displayName === 'string' ? model.displayName : id,
          aliases: strings(model.aliases),
          description: typeof model.description === 'string' ? model.description : undefined,
          reasoningEfforts: efforts,
          defaultReasoning: typeof model.defaultReasoning === 'string' && efforts.includes(model.defaultReasoning)
            ? model.defaultReasoning : undefined,
          reasoningAliases: aliases
        }]
      })
    })
  } catch {
    return []
  }
}

export function findConfiguredModel(value: string | null | undefined, provider?: string | null): CatalogModel | undefined {
  if (!value) return undefined
  const key = value.trim().toLowerCase()
  const matches = configuredModels().filter(model => (!provider || model.provider === provider) &&
    [model.id, `${model.provider}/${model.id}`, ...model.aliases].some(name => name.toLowerCase() === key))
  return matches.length === 1 ? matches[0] : undefined
}

export function configuredModelReasoning(model: CatalogModel, requested?: string | null): string | undefined {
  const value = requested?.trim().toLowerCase() || model.defaultReasoning
  if (!value) return undefined
  const effective = model.reasoningAliases[value] ?? value
  return model.reasoningEfforts.includes(effective) ? effective : model.defaultReasoning
}

export function mentionsSelectionName(text: string, names: string[]): boolean {
  // Quoted/pasted selections are context, not authorization to replace a pinned model.
  const prose = text.replace(/```[\s\S]*?```/g, '').replace(/`([^`]+)`/g, '$1')
    .replace(/^\s*>.*$/gm, '').replace(/"[^"\n]*"|“[^”\n]*”/g, '')
  return names.some(name => name.length > 0 && new RegExp(
    `(?<![a-z0-9])${name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?![a-z0-9])`, 'i'
  ).test(prose))
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string' && item.length > 0) : []
}

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}
