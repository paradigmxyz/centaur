require "cgi"

module ApplicationHelper
  MARKDOWN_ALLOWED_TAGS = %w[
    a blockquote br code del div em h1 h2 h3 h4 li ol p pre strong
    table tbody td th thead tr ul
  ].freeze
  MARKDOWN_ALLOWED_ATTRIBUTES = %w[class href rel target].freeze

  # Truncates a string in the middle with an ellipsis (e.g. "salesforce…rest-api"),
  # keeping the head and tail visible -- useful for opaque ids where both ends
  # carry meaning. Returns the value unchanged when it already fits within +max+.
  def truncate_middle(value, max: 40, omission: "…")
    value = value.to_s
    return value if value.length <= max

    keep = max - omission.length
    head = (keep / 2.0).ceil
    tail = keep / 2
    "#{value[0, head]}#{omission}#{value[-tail, tail]}"
  end

  # Tailwind classes for a broker credential status badge (live / dead /
  # bootstrapping), keyed by BrokerCredential#status. Lives here (not the
  # controller) so Tailwind's content scanner picks up the color classes.
  def credential_status_classes(status)
    case status
    when "live" then "bg-emerald-500/10 text-emerald-300 ring-emerald-500/25"
    when "dead" then "bg-red-500/10 text-red-300 ring-red-500/25"
    else "bg-amber-500/10 text-amber-300 ring-amber-500/25"
    end
  end

  def workflow_status_classes(status)
    case status.to_s
    when "completed" then "border-centaur-500/30 bg-centaur-500/10 text-centaur-300"
    when "running" then "border-sky-500/40 bg-sky-500/10 text-sky-300"
    when "failed" then "border-red-500/40 bg-red-500/10 text-red-300"
    when "cancelled" then "border-ink-600 bg-ink-800/80 text-zinc-400"
    when "pending", "sleeping" then "border-amber-500/40 bg-amber-500/10 text-amber-300"
    else "border-ink-600 bg-ink-800/80 text-zinc-400"
    end
  end

  # Engine names rendered the way the Chats page renders harness types
  # (Console::ThreadsController#thread_harness_label): known harnesses get
  # their product names, anything else is capitalized word-wise.
  def workflow_engine_label(harness_type)
    case harness_type.to_s
    when "codex" then "Codex"
    when "claudecode" then "Claude Code"
    when "amp" then "Amp"
    when "nanocodex" then "Nanocodex"
    when "" then nil
    else harness_type.to_s.tr("_-", " ").squish.split.map(&:capitalize).join(" ")
    end
  end

  # GitHub URL for a workflow source path reported by the workflow host.
  # Paths are repo-relative; an overlay-repo prefix ("centaur-tempo/...") maps
  # to the tempo overlay repo, everything else to the main centaur repo.
  def workflow_source_url(source_path)
    path = source_path.to_s
    return nil if path.blank?

    if path.start_with?("centaur-tempo/")
      "https://github.com/tempoxyz/centaur-tempo/blob/main/#{path.delete_prefix("centaur-tempo/")}"
    else
      "https://github.com/paradigmxyz/centaur/blob/main/#{path}"
    end
  end

  # Human label for a workflow schedule from the workflows API, e.g.
  # "cron */5 * * * *" or "every 5m". The kind is the serde-tagged enum
  # {"type":"cron","cron":...} | {"type":"interval","interval_seconds":...}.
  def workflow_schedule_label(schedule)
    kind = schedule.is_a?(Hash) ? schedule["kind"] : nil
    return nil unless kind.is_a?(Hash)

    case kind["type"]
    when "cron"
      "cron #{kind["cron"]}"
    when "interval"
      seconds = kind["interval_seconds"].to_i
      "every #{seconds % 60 == 0 && seconds >= 60 ? "#{seconds / 60}m" : "#{seconds}s"}"
    end
  end

  # Pretty-printed JSON for workflow run payloads (input/result/failure).
  # Falls back to to_s for values the generator refuses.
  def workflow_debug_json(value)
    JSON.pretty_generate(value)
  rescue JSON::GeneratorError
    value.to_s
  end

  def workflow_duration_label(run)
    started_at = run.started_or_created_at
    finished_at = run.terminal_at
    return "running" if started_at.present? && finished_at.blank? && run.display_status == "running"
    return "—" if started_at.blank? || finished_at.blank?

    distance_of_time_in_words(started_at, finished_at)
  end

  def secret_option_label(secret)
    primary = secret.try(:name).presence || secret.foreign_id.presence || secret.oid
    identifier = secret.foreign_id.presence || secret.oid
    details = [ (identifier unless identifier == primary), secret.namespace ].compact_blank

    details.any? ? "#{primary} (#{details.join(", ")})" : primary
  end

  def console_icon(name, classes: "size-4")
    case name
    when "arrow-up"
      outline_icon(classes, "M4.5 10.5 12 3m0 0 7.5 7.5M12 3v18")
    when "database"
      outline_icon(
        classes,
        "M4.5 6.75c0 1.243 3.358 2.25 7.5 2.25s7.5-1.007 7.5-2.25S16.142 4.5 12 4.5 4.5 5.507 4.5 6.75Zm0 0v10.5c0 1.243 3.358 2.25 7.5 2.25s7.5-1.007 7.5-2.25V6.75M4.5 12c0 1.243 3.358 2.25 7.5 2.25s7.5-1.007 7.5-2.25"
      )
    when "computer"
      outline_icon(
        classes,
        "M9 17.25v1.007a3 3 0 0 1-.879 2.122L7.5 21h9l-.621-.621A3 3 0 0 1 15 18.257V17.25m6-12V15A2.25 2.25 0 0 1 18.75 17.25H5.25A2.25 2.25 0 0 1 3 15V5.25A2.25 2.25 0 0 1 5.25 3h13.5A2.25 2.25 0 0 1 21 5.25Z"
      )
    when "id-badge"
      outline_icon(
        classes,
        "M6.75 3.75h10.5A2.25 2.25 0 0 1 19.5 6v12a2.25 2.25 0 0 1-2.25 2.25H6.75A2.25 2.25 0 0 1 4.5 18V6a2.25 2.25 0 0 1 2.25-2.25ZM9 8.25h6M9 15.75h6M9 12h6"
      )
    when "ellipsis-horizontal"
      tag.svg(
        safe_join([
          tag.circle(cx: "6.75", cy: "12", r: "1"),
          tag.circle(cx: "12", cy: "12", r: "1"),
          tag.circle(cx: "17.25", cy: "12", r: "1")
        ]),
        xmlns: "http://www.w3.org/2000/svg",
        viewBox: "0 0 24 24",
        fill: "currentColor",
        class: classes,
        aria: { hidden: true },
        focusable: "false"
      )
    when "key"
      outline_icon(
        classes,
        "M15.75 7.5a4.5 4.5 0 1 1-1.118 2.966L21 16.834V19.5h-2.666l-1.5-1.5h-2.121l-1.5-1.5v-2.121l-1.179-1.179A4.5 4.5 0 0 1 15.75 7.5Z"
      )
    when "link"
      outline_icon(
        classes,
        "M13.5 6.75h2.25a4.5 4.5 0 0 1 0 9H13.5m-3-9H8.25a4.5 4.5 0 0 0 0 9h2.25M8.25 12h7.5"
      )
    when "log-out"
      outline_icon(
        classes,
        "M15.75 9V5.25A2.25 2.25 0 0 0 13.5 3h-6A2.25 2.25 0 0 0 5.25 5.25v13.5A2.25 2.25 0 0 0 7.5 21h6a2.25 2.25 0 0 0 2.25-2.25V15M12 12h9m0 0-3-3m3 3-3 3"
      )
    when "moon"
      outline_icon(
        classes,
        "M21.752 15.002A9.718 9.718 0 0 1 18 15.75c-5.385 0-9.75-4.365-9.75-9.75 0-1.33.266-2.598.748-3.752A9.753 9.753 0 0 0 3 11.25C3 16.635 7.365 21 12.75 21a9.753 9.753 0 0 0 9.002-5.998Z"
      )
    when "magnifying-glass"
      outline_icon(
        classes,
        "m21 21-5.197-5.197m0 0A7.5 7.5 0 1 0 5.196 5.196a7.5 7.5 0 0 0 10.607 10.607Z"
      )
    when "message-square"
      outline_icon(
        classes,
        "M6.75 5.25h10.5A2.25 2.25 0 0 1 19.5 7.5v6A2.25 2.25 0 0 1 17.25 15.75H10.5L6 19.5v-3.75A2.25 2.25 0 0 1 3.75 13.5v-6A2.25 2.25 0 0 1 6.75 5.25Z"
      )
    when "panel-left"
      outline_icon(
        classes,
        "M4.5 5.25A1.5 1.5 0 0 1 6 3.75h12a1.5 1.5 0 0 1 1.5 1.5v13.5a1.5 1.5 0 0 1-1.5 1.5H6a1.5 1.5 0 0 1-1.5-1.5V5.25ZM9 3.75v16.5"
      )
    when "panel-right"
      outline_icon(
        classes,
        "M4.5 5.25A1.5 1.5 0 0 1 6 3.75h12a1.5 1.5 0 0 1 1.5 1.5v13.5a1.5 1.5 0 0 1-1.5 1.5H6a1.5 1.5 0 0 1-1.5-1.5V5.25ZM15 3.75v16.5"
      )
    when "plus"
      outline_icon(classes, "M12 4.5v15m7.5-7.5h-15")
    when "chevron-right"
      outline_icon(classes, "m8.25 4.5 7.5 7.5-7.5 7.5")
    when "check"
      outline_icon(classes, "m4.5 12.75 6 6 9-13.5")
    when "x-mark"
      outline_icon(classes, "M6 18 18 6M6 6l12 12")
    when "shield-check"
      outline_icon(
        classes,
        "M12 3.75 19.5 6v5.25c0 4.207-2.765 8.04-7.5 9-4.735-.96-7.5-4.793-7.5-9V6L12 3.75Zm3.75 6-4.5 4.5-2.25-2.25"
      )
    when "share"
      outline_icon(
        classes,
        "M12 16.5V3m0 0L7.5 7.5M12 3l4.5 4.5M6.75 10.5h-.75A2.25 2.25 0 0 0 3.75 12.75v6A2.25 2.25 0 0 0 6 21h12a2.25 2.25 0 0 0 2.25-2.25v-6A2.25 2.25 0 0 0 18 10.5h-.75"
      )
    when "slack"
      tag.svg(
        tag.path(
          d: "M5.042 15.165a2.528 2.528 0 0 1-2.52 2.523A2.528 2.528 0 0 1 0 15.165a2.527 2.527 0 0 1 2.522-2.52h2.52v2.52ZM6.313 15.165a2.527 2.527 0 0 1 2.521-2.52 2.527 2.527 0 0 1 2.521 2.52v6.313A2.528 2.528 0 0 1 8.834 24a2.528 2.528 0 0 1-2.52-2.522v-6.313ZM8.834 5.042a2.528 2.528 0 0 1-2.52-2.52A2.528 2.528 0 0 1 8.834 0a2.528 2.528 0 0 1 2.521 2.522v2.52H8.834ZM8.834 6.313a2.528 2.528 0 0 1 2.521 2.521 2.528 2.528 0 0 1-2.521 2.521H2.522A2.528 2.528 0 0 1 0 8.834a2.528 2.528 0 0 1 2.522-2.521h6.312ZM18.956 8.834a2.528 2.528 0 0 1 2.522-2.521A2.528 2.528 0 0 1 24 8.834a2.528 2.528 0 0 1-2.522 2.521h-2.522V8.834ZM17.686 8.834a2.528 2.528 0 0 1-2.522 2.521 2.527 2.527 0 0 1-2.52-2.521V2.522A2.527 2.527 0 0 1 15.164 0a2.528 2.528 0 0 1 2.522 2.522v6.312ZM15.164 18.956a2.528 2.528 0 0 1 2.522 2.522A2.528 2.528 0 0 1 15.164 24a2.527 2.527 0 0 1-2.52-2.522v-2.522h2.52ZM15.164 17.686a2.527 2.527 0 0 1-2.52-2.521 2.527 2.527 0 0 1 2.52-2.52h6.314A2.528 2.528 0 0 1 24 15.165a2.528 2.528 0 0 1-2.522 2.521h-6.314Z"
        ),
        xmlns: "http://www.w3.org/2000/svg",
        viewBox: "0 0 24 24",
        fill: "currentColor",
        class: classes,
        aria: { hidden: true },
        focusable: "false"
      )
    when "sun"
      outline_icon(
        classes,
        "M12 3v2.25M12 18.75V21M4.5 4.5l1.591 1.591M17.909 17.909 19.5 19.5M3 12h2.25M18.75 12H21M4.5 19.5l1.591-1.591M17.909 6.091 19.5 4.5M15.75 12a3.75 3.75 0 1 1-7.5 0 3.75 3.75 0 0 1 7.5 0Z"
      )
    when "user-circle"
      outline_icon(
        classes,
        "M15.75 9.75a3.75 3.75 0 1 1-7.5 0 3.75 3.75 0 0 1 7.5 0ZM4.5 19.5a8.25 8.25 0 1 1 15 0 9.72 9.72 0 0 0-15 0Z"
      )
    when "workflow"
      outline_icon(
        classes,
        "M6 6h3.75v3.75H6V6Zm8.25 8.25H18V18h-3.75v-3.75ZM6 14.25h3.75V18H6v-3.75Zm3.75-6.375H12a3 3 0 0 1 3 3v3.375M9.75 16.125H12a3 3 0 0 0 3-3V9.75"
      )
    when "users"
      outline_icon(
        classes,
        "M9.75 10.5a3.75 3.75 0 1 1 7.5 0 3.75 3.75 0 0 1-7.5 0ZM4.5 18.75a6.75 6.75 0 0 1 13.5 0M18 8.25a3 3 0 0 1 0 6M19.5 18.75a5.25 5.25 0 0 0-2.25-4.307"
      )
    when "menu"
      outline_icon(classes, "M3.75 6.75h16.5M3.75 12h16.5M3.75 17.25h16.5")
    end
  end

  # The brand logo for an OAuth provider as an inline SVG, or nil when we have
  # no logo for it -- callers fall back to showing the provider name as text.
  # Official brand marks keep their own colors (Google's G, Slack's pinwheel);
  # GitHub's mark uses currentColor so it follows the theme.
  def oauth_provider_logo(provider, classes: "size-6")
    paths =
      case provider.to_s
      when "google"
        [
          [ "#4285F4", "M23.52 12.273c0-.851-.076-1.67-.218-2.455H12v4.642h6.458a5.52 5.52 0 0 1-2.394 3.622v3.011h3.878c2.269-2.089 3.578-5.165 3.578-8.82Z" ],
          [ "#34A853", "M12 24c3.24 0 5.956-1.075 7.942-2.907l-3.878-3.011c-1.075.72-2.45 1.145-4.064 1.145-3.125 0-5.771-2.111-6.715-4.948H1.276v3.109A11.995 11.995 0 0 0 12 24Z" ],
          [ "#FBBC05", "M5.285 14.279A7.213 7.213 0 0 1 4.909 12c0-.79.136-1.56.376-2.279V6.612H1.276A11.995 11.995 0 0 0 0 12c0 1.936.464 3.769 1.276 5.388l4.009-3.109Z" ],
          [ "#EA4335", "M12 4.773c1.762 0 3.344.605 4.587 1.794l3.442-3.442C17.951 1.19 15.235 0 12 0 7.31 0 3.253 2.69 1.276 6.612l4.009 3.109C6.229 6.884 8.875 4.773 12 4.773Z" ]
        ]
      when "slack"
        [
          [ "#E01E5A", "M5.042 15.165a2.528 2.528 0 0 1-2.52 2.523A2.528 2.528 0 0 1 0 15.165a2.527 2.527 0 0 1 2.522-2.52h2.52v2.52ZM6.313 15.165a2.527 2.527 0 0 1 2.521-2.52 2.527 2.527 0 0 1 2.521 2.52v6.313A2.528 2.528 0 0 1 8.834 24a2.528 2.528 0 0 1-2.521-2.522v-6.313Z" ],
          [ "#36C5F0", "M8.834 5.042a2.528 2.528 0 0 1-2.521-2.52A2.528 2.528 0 0 1 8.834 0a2.528 2.528 0 0 1 2.521 2.522v2.52H8.834ZM8.834 6.313a2.528 2.528 0 0 1 2.521 2.521 2.528 2.528 0 0 1-2.521 2.521H2.522A2.528 2.528 0 0 1 0 8.834a2.528 2.528 0 0 1 2.522-2.521h6.312Z" ],
          [ "#2EB67D", "M18.956 8.834a2.528 2.528 0 0 1 2.522-2.521A2.528 2.528 0 0 1 24 8.834a2.528 2.528 0 0 1-2.522 2.521h-2.522V8.834ZM17.688 8.834a2.528 2.528 0 0 1-2.523 2.521 2.527 2.527 0 0 1-2.52-2.521V2.522A2.527 2.527 0 0 1 15.165 0a2.528 2.528 0 0 1 2.523 2.522v6.312Z" ],
          [ "#ECB22E", "M15.165 18.956a2.528 2.528 0 0 1 2.523 2.522A2.528 2.528 0 0 1 15.165 24a2.527 2.527 0 0 1-2.52-2.522v-2.522h2.52ZM15.165 17.688a2.527 2.527 0 0 1-2.52-2.523 2.526 2.526 0 0 1 2.52-2.52h6.313A2.527 2.527 0 0 1 24 15.165a2.528 2.528 0 0 1-2.522 2.523h-6.313Z" ]
        ]
      when "github"
        [
          [ "currentColor", "M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12" ]
        ]
      when "granola"
        [
          [ "currentColor", "M15.83 31.91C19.13 31.91 22.59 31.18 23.98 30.17C24.87 29.53 25.31 29.6 26.1 28.84C26.32 28.62 26.42 28.55 26.48 28.49C29.5 26.01 31.24 22.81 31.24 18.65C31.24 11.89 26.45 7.29 19.6 7.29C13.57 7.29 8.97 11.13 8.97 16.05C8.97 20.52 12.46 23.73 17.44 23.73C17.73 23.73 17.85 23.57 18.17 23.57C19.38 23.57 20.36 23.22 21 22.55C21.31 22.2 21.92 21.5 21.98 21.47C22.27 21.22 22.33 20.84 22.39 20.68C22.45 20.52 22.58 20.42 22.65 20.2C22.71 19.95 22.62 19.63 22.62 19.35C22.62 18.84 22.84 18.33 22.84 17.86C22.84 16.53 21.25 15.19 19.82 15.19C19.66 15.19 19.66 15.06 19.57 15.06C19.47 15.06 19.38 15.16 19.28 15.16C19.18 15.16 19.06 15 18.93 15C18.8 15 18.74 15.13 18.55 15.13C18.14 15.13 18.11 15.19 17.79 15.19C17.68 15.2 17.58 15.22 17.47 15.25C17.35 15.32 17.35 15.41 17.22 15.41C17.05 15.41 16.96 15.45 16.96 15.51C16.96 15.86 17 15.8 16.84 15.8C16.67 15.8 16.56 15.81 16.52 15.83C16.39 15.89 16.49 16.05 16.36 16.14C16.23 16.21 16.2 16.3 16.17 16.49C16.14 16.68 15.95 16.75 15.95 16.94C15.95 17.03 15.98 17.1 15.98 17.16C15.98 17.35 15.6 17.29 15.6 17.48C15.6 17.6 15.7 17.7 15.7 17.82C15.7 17.92 15.64 17.98 15.64 18.08C15.64 18.18 15.7 18.24 15.7 18.3C15.7 18.46 15.48 18.49 15.48 18.62C15.48 18.74 15.61 18.84 15.61 18.93C15.61 19 15.51 19.03 15.51 19.12C15.51 19.22 15.48 19.06 15.67 19.34C15.79 19.53 15.79 19.63 15.67 19.79C15.54 19.95 15.29 20.05 14.97 20.05C13.64 20.05 13.48 18.68 12.75 18.46C12.53 18.4 12.49 18.36 12.49 18.27C12.49 18.18 12.49 18.21 12.62 18.08C12.75 17.95 12.78 17.86 12.78 17.76C12.78 17.67 12.78 17.63 12.72 17.57C12.46 17.16 12.34 16.65 12.34 16.08C12.34 13.16 15.89 10.72 19.26 10.72C20.27 10.72 20.11 10.97 20.72 10.97C20.88 10.97 20.81 10.97 21.03 10.94C21.61 10.85 22.65 11.07 23.48 11.48C25.76 12.62 27.25 15.32 27.25 18.46C27.25 23.83 22.49 27.73 16.15 27.73C12.69 27.73 10.31 26.65 7.96 24.01C7.7 23.73 8.09 24.08 7.67 23.35C7.17 22.46 7.23 23.03 7.23 23.03C7.07 22.84 6.78 22.3 6.62 22.11C6.43 21.89 6.18 21.92 6.09 21.79C5.96 21.64 6.15 21.35 6.09 21.19C6.02 20.94 5.48 20.56 5.42 20.37C5.36 20.18 5.13 18.97 5.13 18.75C5.13 18.49 5.32 18.46 5.32 18.3C5.32 18.08 5.04 18.02 4.88 17.7C4.72 17.38 4.62 16.65 4.62 15.86C4.62 15.44 4.62 15.29 4.75 14.33C4.78 14.02 5.26 14.02 5.26 13.67C5.26 13.54 5.2 13.38 5.2 13.29C5.2 13.16 5.2 13.13 5.23 13.03C6.56 7.45 12.53 3.29 19.16 3.29C21.44 3.29 23.19 3.71 25.88 4.85C26.77 5.23 28.07 4.56 28.07 3.87C28.14 3.65 28.01 3.58 27.98 3.45C27.95 3.32 27.82 3.17 27.69 3.13C27.63 3.1 27.59 3.01 27.53 2.91C27.43 2.76 27.34 2.69 27.09 2.63C26.99 2.61 26.9 2.57 26.83 2.5C26.76 2.39 26.66 2.3 26.55 2.25C26.42 2.18 26.32 2.25 26.26 2.21C26.2 2.18 26.16 2.12 26.1 2.09C26.07 2.06 26 2.06 25.91 2.06C22.71 0.34 20.62 0.09 17.83 0.09C11.8 0.09 6.4 2.69 3.07 7.23C2.79 7.61 2.94 8.24 2.53 8.62C1.74 9.35 0.76 13.35 0.76 15.89C0.76 18.01 1.26 20.84 1.93 22.39C3.23 25.44 2.66 24.17 2.85 24.46C3.23 25.06 3.55 25.12 3.71 25.31C3.71 25.31 3.8 25.5 3.8 25.69C3.8 25.82 3.8 25.85 3.83 25.91C3.93 26.1 4.37 26.42 4.5 26.55C4.79 26.83 5.01 27.44 5.48 27.91C6.21 28.64 7.26 29.21 10.59 30.77C11.76 31.31 11.1 31.02 11.22 31.05C11.51 31.15 11.89 31.15 12.12 31.31C12.24 31.41 12.08 31.37 12.43 31.37C12.49 31.37 12.49 31.4 12.56 31.43C12.62 31.46 12.69 31.56 12.78 31.56C12.84 31.56 12.88 31.5 12.97 31.53C13.06 31.56 13.29 31.72 13.48 31.82C13.64 31.91 13.67 31.91 13.73 31.85C13.92 31.72 14.05 31.88 14.24 31.88C14.3 31.88 14.4 31.85 14.59 31.85C14.84 31.85 14.84 31.91 15.83 31.91" ]
        ]
      when "attio"
        [
          [ "currentColor", "M30.65 17.78L28.06 13.64C28.06 13.64 28.05 13.62 28.04 13.62L27.84 13.29C27.46 12.67 26.79 12.3 26.06 12.3L21.89 12.29L21.6 12.75L16.62 20.72L16.35 21.16L18.44 24.5C18.82 25.12 19.49 25.49 20.22 25.49H26.06C26.78 25.49 27.46 25.11 27.84 24.5L28.05 24.17C28.05 24.17 28.06 24.16 28.06 24.16L30.65 20.01C31.07 19.33 31.07 18.46 30.65 17.78H30.65ZM29.86 19.52L27.27 23.66C27.26 23.68 27.24 23.7 27.23 23.71C27.14 23.81 27.02 23.83 26.97 23.83C26.91 23.83 26.76 23.81 26.67 23.66L24.08 19.51C24.05 19.47 24.03 19.42 24 19.37C23.98 19.32 23.96 19.27 23.95 19.22C23.89 19.01 23.89 18.79 23.95 18.58C23.98 18.48 24.02 18.37 24.08 18.28L26.66 14.14C26.66 14.14 26.67 14.13 26.67 14.13C26.73 14.04 26.81 13.99 26.88 13.98C26.9 13.97 26.93 13.97 26.95 13.97C26.96 13.97 26.96 13.97 26.97 13.97C27.03 13.97 27.18 13.99 27.27 14.14L29.86 18.28C30.1 18.65 30.1 19.14 29.86 19.52H29.86Z" ],
          [ "currentColor", "M22.99 7.76C23.41 7.08 23.41 6.21 22.99 5.54L20.4 1.4L20.19 1.05C19.8 0.43 19.14 0.06 18.4 0.06H12.56C11.84 0.06 11.17 0.43 10.78 1.05L0.32 17.78C0.11 18.12 0 18.51 0 18.9C0 19.29 0.11 19.68 0.32 20.01L3.13 24.5C3.51 25.12 4.18 25.49 4.91 25.49H10.75C11.48 25.49 12.15 25.12 12.53 24.5L12.75 24.16C12.75 24.16 12.75 24.16 12.75 24.16C12.75 24.16 12.75 24.15 12.75 24.15L14.83 20.82L21.01 10.93L22.99 7.77L22.99 7.76ZM22.38 6.65C22.38 6.86 22.32 7.08 22.2 7.27L11.96 23.66C11.86 23.81 11.72 23.83 11.66 23.83C11.6 23.83 11.45 23.81 11.36 23.66L8.77 19.52C8.53 19.14 8.53 18.66 8.77 18.28L19.01 1.89C19.1 1.74 19.25 1.72 19.31 1.72C19.37 1.72 19.52 1.74 19.61 1.89L22.2 6.03C22.32 6.22 22.38 6.44 22.38 6.65V6.65Z" ]
        ]
      when "linear"
        [
          [ "#5E6AD2", "M2.886 4.18A11.982 11.982 0 0 1 11.99 0C18.624 0 24 5.376 24 12.009c0 3.64-1.62 6.903-4.18 9.105L2.887 4.18ZM1.817 5.626l16.556 16.556c-.524.33-1.075.62-1.65.866L.951 7.277c.247-.575.537-1.126.866-1.65ZM.322 9.163l14.515 14.515c-.71.172-1.443.282-2.195.322L0 11.358a12 12 0 0 1 .322-2.195Zm-.17 4.862 9.823 9.824a12.02 12.02 0 0 1-9.824-9.824Z" ]
        ]
      end
    return nil unless paths

    viewbox =
      case provider.to_s
      when "attio" then "0 0 31.08 25.55"
      when "granola" then "0 0 32 32"
      else "0 0 24 24"
      end

    tag.svg(
      safe_join(paths.map { |fill, d| tag.path(fill: fill, d: d) }),
      xmlns: "http://www.w3.org/2000/svg",
      viewBox: viewbox,
      class: classes,
      aria: { hidden: true },
      focusable: "false"
    )
  end

  def console_markdown(text)
    sanitize(
      markdown_blocks(text.to_s).join,
      tags: MARKDOWN_ALLOWED_TAGS,
      attributes: MARKDOWN_ALLOWED_ATTRIBUTES
    )
  end

  def console_sidebar_thread_title(session, latest_message = nil)
    # sessions.title is the title api-rs generates on message append; prefer it
    # over metadata heuristics. Guarded because snapshots mirrored before the
    # title migration have no such column.
    stored = session.title.presence if session.respond_to?(:title)
    return console_sidebar_clip_one_line(stored, 48) if stored

    metadata = session.metadata_hash
    summary = metadata["summary"]
    title = metadata["title"].presence ||
      metadata["generated_title"].presence ||
      metadata["summary_title"].presence ||
      metadata["thread_title"].presence ||
      (metadata["thread"].is_a?(Hash) ? metadata["thread"]["title"] : nil).presence ||
      (metadata["summary"].is_a?(Hash) ? metadata["summary"]["title"] : nil).presence ||
      (summary if summary.is_a?(String)).presence ||
      metadata["subject"].presence ||
      metadata["issue_title"].presence
    return console_sidebar_generated_thread_title(title) if title

    generated = console_sidebar_generated_thread_title(console_sidebar_thread_message_text(latest_message))
    return generated if generated.present?

    truncate_middle(session.thread_key, max: 42)
  end

  def console_sidebar_thread_message_text(message)
    return "" unless message

    message.parts_array.filter_map do |part|
      next unless part.is_a?(Hash)

      case part["type"]
      when "text" then part["text"].to_s
      when "image" then "[image]"
      when "document" then "[document]"
      end
    end.join("\n").squish
  end

  def console_sidebar_generated_thread_title(text)
    title = text.to_s
      .gsub(/<@[A-Z0-9]+(?:\|[^>]+)?>/, "")
      .sub(/\A\s*@?centaur\b[:,]?\s*/i, "")
      .sub(/\A\s*@?U[A-Z0-9]+\b[:,]?\s*/i, "")
      .sub(/\A\s*@\S+\s+/, "")
      .strip
    title = title.sub(/\A[*_]{1,2}(.+?)[*_]{1,2}\s*/, "\\1 ").squish
    console_sidebar_clip_one_line(title, 48)
  end

  # The broker credential a record wraps when it is an OAuth-flow-managed static
  # secret; nil for ordinary secrets and for non-static kinds. Drives the "managed"
  # badge and the credential <-> secret cross-links. Lives in a helper (not a
  # controller helper_method) so it is available to both the ConsoleController
  # views and the Console::BaseSecretsController edit form.
  def managed_credential(record)
    return nil unless record.respond_to?(:broker_credential)
    record.broker_credential
  end

  # The muted secondary line shown under a record's primary identifier in console
  # tables: the namespace, optionally preceded by the opaque oid and a small dot.
  # Pass oid: when the primary line is the foreign_id (so the oid still shows);
  # omit it when the oid is already the primary line.
  def id_meta_line(namespace, oid: nil)
    inner =
      if oid
        safe_join([ oid, tag.span("·", class: "mx-1 text-zinc-600"), namespace ])
      else
        namespace
      end
    tag.div(inner, class: "text-xs text-zinc-500")
  end

  # Renders a UTC timestamp that the `localtime` Stimulus controller rewrites in
  # the viewer's local time zone. With relative: true it shows a "5 minutes ago"
  # style string (absolute local time on hover). Pass format: :compact with
  # relative: true for short labels like "4d" or "1mo". The ISO-8601 text is the
  # pre-JS / no-JS fallback. Returns an em-dash placeholder for nil.
  def local_time(time, relative: false, format: nil)
    return tag.span("—", class: "text-zinc-600") if time.nil?

    iso = time.utc.iso8601
    data = {
      controller: "localtime",
      localtime_datetime_value: iso,
      localtime_relative_value: relative
    }
    data[:localtime_format_value] = format.to_s if format.present?

    tag.time(
      iso,
      datetime: iso,
      data: data
    )
  end

  def outline_icon(classes, path)
    tag.svg(
      tag.path(d: path, "stroke-linecap": "round", "stroke-linejoin": "round"),
      xmlns: "http://www.w3.org/2000/svg",
      fill: "none",
      viewBox: "0 0 24 24",
      "stroke-width": "1.8",
      stroke: "currentColor",
      class: classes,
      aria: { hidden: true },
      focusable: "false"
    )
  end

  def markdown_blocks(raw_text)
    lines = raw_text.to_s.gsub("\r\n", "\n").split("\n", -1)
    blocks = []
    index = 0

    while index < lines.length
      line = lines[index]
      start_index = index

      if line.blank?
        index += 1
      elsif line.start_with?("```")
        code_lines = []
        index += 1
        while index < lines.length && !lines[index].start_with?("```")
          code_lines << lines[index]
          index += 1
        end
        index += 1 if index < lines.length
        blocks << %(<pre class="overflow-x-auto rounded bg-ink-950/80 p-3 text-xs leading-5 text-zinc-200"><code>#{ERB::Util.html_escape(code_lines.join("\n"))}</code></pre>)
      elsif (heading = line.match(/\A(\#{1,4})\s+(.+)\z/))
        level = heading[1].length
        classes = "mb-2 mt-4 text-sm font-semibold text-zinc-100 first:mt-0"
        blocks << %(<h#{level} class="#{classes}">#{markdown_inline(heading[2])}</h#{level}>)
        index += 1
      elsif line.match?(/\A\s*[-*+]\s+/)
        items = []
        while index < lines.length && (item = lines[index].match(/\A\s*[-*+]\s+(.+)\z/))
          items << item[1]
          index += 1
        end
        blocks << %(<ul class="mb-3 list-disc space-y-1 pl-5 last:mb-0">#{items.map { |item| %(<li>#{markdown_inline(item)}</li>) }.join}</ul>)
      elsif line.match?(/\A\s*\d+\.\s+/)
        items = []
        while index < lines.length && (item = lines[index].match(/\A\s*\d+\.\s+(.+)\z/))
          items << item[1]
          index += 1
        end
        blocks << %(<ol class="mb-3 list-decimal space-y-1 pl-5 last:mb-0">#{items.map { |item| %(<li>#{markdown_inline(item)}</li>) }.join}</ol>)
      elsif line.match?(/\A\s*>\s?/)
        quoted = []
        while index < lines.length && (quote = lines[index].match(/\A\s*>\s?(.*)\z/))
          quoted << quote[1]
          index += 1
        end
        blocks << %(<blockquote class="mb-3 border-l border-ink-500 pl-3 text-zinc-400 last:mb-0">#{markdown_inline(quoted.join(" "))}</blockquote>)
      elsif markdown_table_row?(line) && markdown_table_separator?(lines[index + 1])
        header = markdown_table_cells(line)
        alignments = markdown_table_alignments(lines[index + 1])
        index += 2
        rows = []
        while index < lines.length && markdown_table_row?(lines[index])
          rows << markdown_table_cells(lines[index])
          index += 1
        end
        blocks << markdown_table(header, alignments, rows)
      else
        paragraph = []
        while index < lines.length && lines[index].present? && !markdown_block_start?(lines[index])
          paragraph << lines[index]
          index += 1
        end
        # Empty when the line is a table row without a separator: the progress
        # guard below emits it as its own paragraph.
        blocks << %(<p class="mb-3 last:mb-0">#{markdown_inline(paragraph.join(" "))}</p>) unless paragraph.empty?
      end

      # Guarantee forward progress: a block-start marker with no content (e.g.
      # a bare "- ", "1. ", or "# ") matches a branch guard but not its inner
      # consuming regex, which would otherwise spin this loop forever. Emit such
      # a line as an escaped paragraph and advance.
      if index == start_index
        blocks << %(<p class="mb-3 last:mb-0">#{markdown_inline(line)}</p>)
        index += 1
      end
    end

    blocks
  end

  def markdown_block_start?(line)
    line.start_with?("```") ||
      line.match?(/\A\#{1,4}\s+/) ||
      line.match?(/\A\s*[-*+]\s+/) ||
      line.match?(/\A\s*\d+\.\s+/) ||
      line.match?(/\A\s*>\s?/) ||
      markdown_table_row?(line)
  end

  def markdown_table_row?(line)
    stripped = line.to_s.strip
    stripped.start_with?("|") && stripped.length > 1
  end

  def markdown_table_separator?(line)
    return false unless line && markdown_table_row?(line)

    cells = markdown_table_cells(line)
    cells.any? && cells.all? { |cell| cell.match?(/\A:?-+:?\z/) }
  end

  def markdown_table_cells(line)
    inner = line.strip.delete_prefix("|").delete_suffix("|")
    inner.split(/(?<!\\)\|/, -1).map { |cell| cell.strip.gsub("\\|", "|") }
  end

  def markdown_table_alignments(separator_line)
    markdown_table_cells(separator_line).map do |cell|
      left = cell.start_with?(":")
      right = cell.end_with?(":")
      if left && right
        "text-center"
      elsif right
        "text-right"
      else
        "text-left"
      end
    end
  end

  # Extra body cells beyond the header width are dropped and short rows are
  # padded with empty cells, matching GFM table semantics.
  def markdown_table(header, alignments, rows)
    head_cells = header.each_with_index.map do |cell, column|
      %(<th class="border-b border-ink-700 px-3 py-1.5 font-semibold text-zinc-100 #{alignments[column] || "text-left"}">#{markdown_inline(cell)}</th>)
    end
    body_rows = rows.map do |row|
      cells = Array.new(header.length) do |column|
        %(<td class="border-b border-ink-800/70 px-3 py-1.5 align-top #{alignments[column] || "text-left"}">#{markdown_inline(row[column].to_s)}</td>)
      end
      "<tr>#{cells.join}</tr>"
    end
    %(<div class="mb-3 overflow-x-auto last:mb-0"><table class="w-full border-collapse text-sm"><thead><tr>#{head_cells.join}</tr></thead><tbody>#{body_rows.join}</tbody></table></div>)
  end

  def markdown_inline(raw_text)
    text = ERB::Util.html_escape(raw_text.to_s)
    placeholders = []

    text = text.gsub(/`([^`\n]+)`/) do
      markdown_placeholder(placeholders, %(<code class="rounded bg-ink-800 px-1 py-0.5 text-[0.92em] text-zinc-100">#{Regexp.last_match(1)}</code>))
    end
    text = text.gsub(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/i) do
      markdown_placeholder(
        placeholders,
        markdown_link(Regexp.last_match(1), CGI.unescapeHTML(Regexp.last_match(2)))
      )
    end
    text = text.gsub(%r{(?<!["'=])https?://[^\s<]+}i) do |match|
      trailing = match[/[)\].,!?:;]+\z/].to_s
      url = trailing.present? ? match.delete_suffix(trailing) : match
      "#{markdown_link(ERB::Util.html_escape(url), CGI.unescapeHTML(url))}#{ERB::Util.html_escape(trailing)}"
    end

    text = text.gsub(/\*\*([^*\n]+)\*\*/, '<strong class="font-semibold text-zinc-100">\1</strong>')
    text = text.gsub(/__([^_\n]+)__/, '<strong class="font-semibold text-zinc-100">\1</strong>')
    text = text.gsub(/~~([^~\n]+)~~/, '<del>\1</del>')
    text = text.gsub(/(?<!\*)\*([^*\n]+)\*(?!\*)/, '<em>\1</em>')
    text = text.gsub(/(?<!_)_([^_\n]+)_(?!_)/, '<em>\1</em>')

    placeholders.each_with_index do |html, offset|
      text = text.gsub(markdown_token(offset), html)
    end

    text
  end

  def markdown_link(label, url)
    unless url.to_s.match?(/\Ahttps?:\/\/[^\s<>"']+\z/i)
      return label
    end

    href = ERB::Util.html_escape(url)
    %(<a class="console-markdown-link text-sky-300 underline decoration-sky-400/30 underline-offset-2 hover:text-sky-200" href="#{href}" target="_blank" rel="noopener noreferrer">#{label}</a>)
  end

  def markdown_placeholder(placeholders, html)
    placeholders << html
    markdown_token(placeholders.length - 1)
  end

  def markdown_token(offset)
    "%%MDPH#{offset}%%"
  end

  def console_sidebar_clip_one_line(value, max)
    one_line = value.to_s.gsub(/\s+/, " ").strip
    return one_line if one_line.length <= max

    "#{one_line.slice(0, [ max - 3, 0 ].max).rstrip}..."
  end
end
