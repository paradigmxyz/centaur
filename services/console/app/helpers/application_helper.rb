module ApplicationHelper
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

  # Product names for known workflow harnesses; unknown values are capitalized
  # word-wise.
  def workflow_engine_label(harness_type)
    case harness_type.to_s
    when "codex" then "Codex"
    when "claudecode" then "Claude Code"
    when "amp" then "Amp"
    when "nanocodex" then "Nanocodex"
    when "pi" then "Pi"
    when "omp" then "OMP"
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
    details = [ (identifier unless identifier == primary) ].compact_blank

    details.any? ? "#{primary} (#{details.join(", ")})" : primary
  end

  def console_icon(name, classes: "size-4")
    case name
    when "computer"
      outline_icon(
        classes,
        "M9 17.25v1.007a3 3 0 0 1-.879 2.122L7.5 21h9l-.621-.621A3 3 0 0 1 15 18.257V17.25m6-12V15A2.25 2.25 0 0 1 18.75 17.25H5.25A2.25 2.25 0 0 1 3 15V5.25A2.25 2.25 0 0 1 5.25 3h13.5A2.25 2.25 0 0 1 21 5.25Z"
      )
    when "clock"
      outline_icon(
        classes,
        "M12 6v6h4.5m4.5 0a9 9 0 1 1-18 0 9 9 0 0 1 18 0Z"
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
    when "shield-check"
      outline_icon(
        classes,
        "M12 3.75 19.5 6v5.25c0 4.207-2.765 8.04-7.5 9-4.735-.96-7.5-4.793-7.5-9V6L12 3.75Zm3.75 6-4.5 4.5-2.25-2.25"
      )
    when "trash"
      outline_icon(
        classes,
        "M3.75 6h16.5M9.75 6V4.5A1.5 1.5 0 0 1 11.25 3h1.5a1.5 1.5 0 0 1 1.5 1.5V6m3.75 0-.75 13.5A1.5 1.5 0 0 1 15.75 21h-7.5a1.5 1.5 0 0 1-1.5-1.5L6 6m4.5 4.5v6m3-6v6"
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

  # The broker credential a record wraps when it is an OAuth-flow-managed static
  # secret; nil for ordinary secrets and for non-static kinds. Drives the "managed"
  # badge and the credential <-> secret cross-links. Lives in a helper (not a
  # controller helper_method) so it is available to both the ConsoleController
  # views and the Console::BaseSecretsController edit form.
  def managed_credential(record)
    return nil unless record.respond_to?(:broker_credential)
    record.broker_credential
  end

  # The muted opaque-id line shown when a table uses foreign_id as its primary label.
  def id_meta_line(oid)
    tag.div(oid, class: "text-xs text-zinc-500")
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
end
