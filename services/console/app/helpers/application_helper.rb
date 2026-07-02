require "cgi"

module ApplicationHelper
  MARKDOWN_ALLOWED_TAGS = %w[
    a blockquote br code del em h1 h2 h3 h4 li ol p pre strong ul
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
    when "x-mark"
      outline_icon(classes, "M6 18 18 6M6 6l12 12")
    when "shield-check"
      outline_icon(
        classes,
        "M12 3.75 19.5 6v5.25c0 4.207-2.765 8.04-7.5 9-4.735-.96-7.5-4.793-7.5-9V6L12 3.75Zm3.75 6-4.5 4.5-2.25-2.25"
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
    when "users"
      outline_icon(
        classes,
        "M9.75 10.5a3.75 3.75 0 1 1 7.5 0 3.75 3.75 0 0 1-7.5 0ZM4.5 18.75a6.75 6.75 0 0 1 13.5 0M18 8.25a3 3 0 0 1 0 6M19.5 18.75a5.25 5.25 0 0 0-2.25-4.307"
      )
    end
  end

  def console_markdown(text)
    sanitize(
      markdown_blocks(text.to_s).join,
      tags: MARKDOWN_ALLOWED_TAGS,
      attributes: MARKDOWN_ALLOWED_ATTRIBUTES
    )
  end

  def console_sidebar_thread_title(session, latest_message = nil)
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
      else
        paragraph = []
        while index < lines.length && lines[index].present? && !markdown_block_start?(lines[index])
          paragraph << lines[index]
          index += 1
        end
        blocks << %(<p class="mb-3 last:mb-0">#{markdown_inline(paragraph.join(" "))}</p>)
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
      line.match?(/\A\s*>\s?/)
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
