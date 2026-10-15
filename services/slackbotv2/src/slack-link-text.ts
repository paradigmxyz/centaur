const GENERIC_ARCHIVE_PREFIX = 'https://slack.com/archives/'

/** Qualify archive links without buffering the rest of a streamed answer. */
export class SlackLinkTextFilter {
  private pending = ''
  private readonly archivePrefix: string

  constructor(workspaceUrl?: string) {
    this.archivePrefix = GENERIC_ARCHIVE_PREFIX
    if (!workspaceUrl) return
    try {
      const url = new URL(workspaceUrl)
      if (url.protocol === 'https:' && /^[a-z0-9-]+(?:\.enterprise)?\.slack\.com$/.test(url.hostname)) {
        this.archivePrefix = `${url.origin}/archives/`
      }
    } catch {
      // Leave links intact if Slack did not return a usable workspace URL.
    }
  }

  append(text: string): string {
    if (this.archivePrefix === GENERIC_ARCHIVE_PREFIX) return text
    text = this.pending + text
    this.pending = ''
    // A URL prefix may span any number of model deltas. Retain only the suffix
    // that could still become the generic prefix on the next append.
    let length = Math.min(text.length, GENERIC_ARCHIVE_PREFIX.length - 1)
    while (length > 0 && !GENERIC_ARCHIVE_PREFIX.startsWith(text.slice(-length))) length--
    if (length > 0) {
      this.pending = text.slice(-length)
      text = text.slice(0, -length)
    }
    return text.replaceAll(GENERIC_ARCHIVE_PREFIX, this.archivePrefix)
  }

  finish(): string {
    const text = this.pending
    this.pending = ''
    return text
  }
}

export function qualifySlackMessageLinks(text: string, workspaceUrl?: string): string {
  const filter = new SlackLinkTextFilter(workspaceUrl)
  return filter.append(text) + filter.finish()
}
