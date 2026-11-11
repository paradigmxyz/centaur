require "cgi"

# Renders skill instructions as sanitized HTML for the console SPA
# (`content_html` in the /api/v1/skills show payload).
#
# A deliberately small GitHub-flavoured subset: code fences, h1-h4, bullet and
# numbered lists, blockquotes, tables with column alignment, inline code,
# bold/italic/strikethrough, and http(s) links (explicit or bare URLs), which
# open in a new tab with rel="noopener noreferrer". Every source character is
# HTML-escaped first and the result passes an allow-list sanitizer, so the
# output is safe to insert with innerHTML. Styling is left to the SPA (the
# `.console-markdown` container); the only classes emitted are
# `console-markdown-link` and the table alignment classes.
module SkillMarkdown
  ALLOWED_TAGS = %w[
    a blockquote br code del div em h1 h2 h3 h4 li ol p pre strong
    table tbody td th thead tr ul
  ].freeze
  ALLOWED_ATTRIBUTES = %w[class href rel target].freeze
  PLACEHOLDER = /%%MDPH(\d+)%%/

  module_function

  def render(text)
    Rails::HTML5::SafeListSanitizer.new.sanitize(
      blocks(text.to_s).join,
      tags: ALLOWED_TAGS,
      attributes: ALLOWED_ATTRIBUTES
    )
  end

  def blocks(raw_text)
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
        blocks << "<pre><code>#{ERB::Util.html_escape(code_lines.join("\n"))}</code></pre>"
      elsif (heading = line.match(/\A(\#{1,4})\s+(.+)\z/))
        level = heading[1].length
        blocks << "<h#{level}>#{inline(heading[2])}</h#{level}>"
        index += 1
      elsif line.match?(/\A\s*[-*+]\s+/)
        items = []
        while index < lines.length && (item = lines[index].match(/\A\s*[-*+]\s+(.+)\z/))
          items << item[1]
          index += 1
        end
        blocks << "<ul>#{items.map { |item| "<li>#{inline(item)}</li>" }.join}</ul>"
      elsif line.match?(/\A\s*\d+\.\s+/)
        items = []
        while index < lines.length && (item = lines[index].match(/\A\s*\d+\.\s+(.+)\z/))
          items << item[1]
          index += 1
        end
        blocks << "<ol>#{items.map { |item| "<li>#{inline(item)}</li>" }.join}</ol>"
      elsif line.match?(/\A\s*>\s?/)
        quoted = []
        while index < lines.length && (quote = lines[index].match(/\A\s*>\s?(.*)\z/))
          quoted << quote[1]
          index += 1
        end
        blocks << "<blockquote>#{inline(quoted.join(" "))}</blockquote>"
      elsif table_row?(line) && table_separator?(lines[index + 1])
        header = table_cells(line)
        alignments = table_alignments(lines[index + 1])
        index += 2
        rows = []
        while index < lines.length && table_row?(lines[index])
          rows << table_cells(lines[index])
          index += 1
        end
        blocks << table(header, alignments, rows)
      else
        paragraph = []
        while index < lines.length && lines[index].present? && !block_start?(lines[index])
          paragraph << lines[index]
          index += 1
        end
        # Empty when the line is a table row without a separator: the progress
        # guard below emits it as its own paragraph.
        blocks << "<p>#{inline(paragraph.join(" "))}</p>" unless paragraph.empty?
      end

      # Guarantee forward progress: a block-start marker with no content (e.g.
      # a bare "- ", "1. ", or "# ") matches a branch guard but not its inner
      # consuming regex, which would otherwise spin this loop forever. Emit such
      # a line as an escaped paragraph and advance.
      if index == start_index
        blocks << "<p>#{inline(line)}</p>"
        index += 1
      end
    end

    blocks
  end

  def block_start?(line)
    line.start_with?("```") ||
      line.match?(/\A\#{1,4}\s+/) ||
      line.match?(/\A\s*[-*+]\s+/) ||
      line.match?(/\A\s*\d+\.\s+/) ||
      line.match?(/\A\s*>\s?/) ||
      table_row?(line)
  end

  def table_row?(line)
    stripped = line.to_s.strip
    stripped.start_with?("|") && stripped.length > 1
  end

  def table_separator?(line)
    return false unless line && table_row?(line)

    cells = table_cells(line)
    cells.any? && cells.all? { |cell| cell.match?(/\A:?-+:?\z/) }
  end

  def table_cells(line)
    inner = line.strip.delete_prefix("|").delete_suffix("|")
    inner.split(/(?<!\\)\|/, -1).map { |cell| cell.strip.gsub("\\|", "|") }
  end

  def table_alignments(separator_line)
    table_cells(separator_line).map do |cell|
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
  def table(header, alignments, rows)
    head_cells = header.each_with_index.map do |cell, column|
      %(<th class="#{alignments[column] || "text-left"}">#{inline(cell)}</th>)
    end
    body_rows = rows.map do |row|
      cells = Array.new(header.length) do |column|
        %(<td class="#{alignments[column] || "text-left"}">#{inline(row[column].to_s)}</td>)
      end
      "<tr>#{cells.join}</tr>"
    end
    "<div><table><thead><tr>#{head_cells.join}</tr></thead><tbody>#{body_rows.join}</tbody></table></div>"
  end

  def inline(raw_text)
    text = ERB::Util.html_escape(raw_text.to_s)
    placeholders = []

    text = text.gsub(/`([^`\n]+)`/) do
      placeholder(placeholders, "<code>#{Regexp.last_match(1)}</code>")
    end
    text = text.gsub(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/i) do
      placeholder(placeholders, link(Regexp.last_match(1), CGI.unescapeHTML(Regexp.last_match(2))))
    end
    text = text.gsub(%r{(?<!["'=])https?://[^\s<]+}i) do |match|
      trailing = match[/[)\].,!?:;]+\z/].to_s
      url = trailing.present? ? match.delete_suffix(trailing) : match
      "#{link(ERB::Util.html_escape(url), CGI.unescapeHTML(url))}#{ERB::Util.html_escape(trailing)}"
    end

    text = text.gsub(/\*\*([^*\n]+)\*\*/, '<strong>\1</strong>')
    text = text.gsub(/__([^_\n]+)__/, '<strong>\1</strong>')
    text = text.gsub(/~~([^~\n]+)~~/, '<del>\1</del>')
    text = text.gsub(/(?<!\*)\*([^*\n]+)\*(?!\*)/, '<em>\1</em>')
    text = text.gsub(/(?<!_)_([^_\n]+)_(?!_)/, '<em>\1</em>')

    text.gsub(PLACEHOLDER) { |token| placeholders[Regexp.last_match(1).to_i] || token }
  end

  def link(label, url)
    return label unless url.to_s.match?(/\Ahttps?:\/\/[^\s<>"']+\z/i)

    href = ERB::Util.html_escape(url)
    %(<a class="console-markdown-link" href="#{href}" target="_blank" rel="noopener noreferrer">#{label}</a>)
  end

  def placeholder(placeholders, html)
    placeholders << html
    "%%MDPH#{placeholders.length - 1}%%"
  end
end
