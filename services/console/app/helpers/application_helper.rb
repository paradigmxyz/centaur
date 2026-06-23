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
  # style string (absolute local time on hover). The ISO-8601 text is the
  # pre-JS / no-JS fallback. Returns an em-dash placeholder for nil.
  def local_time(time, relative: false)
    return tag.span("—", class: "text-zinc-600") if time.nil?

    iso = time.utc.iso8601
    tag.time(
      iso,
      datetime: iso,
      data: {
        controller: "localtime",
        localtime_datetime_value: iso,
        localtime_relative_value: relative
      }
    )
  end
end
