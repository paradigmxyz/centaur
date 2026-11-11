require "test_helper"
require "timeout"

# Ported from the console_markdown helper tests (application_helper_test.rb).
class SkillMarkdownTest < ActiveSupport::TestCase
  include Rails::Dom::Testing::Assertions::SelectorAssertions

  test "renders common github-flavored markdown" do
    html = SkillMarkdown.render(<<~MARKDOWN)
      Yes, **partially legit**.

      Issue 1 is real on current `main`.

      - one
      - two

      https://github.com/paradigmxyz/centaur/issues/792
    MARKDOWN

    assert_select_in html, "p", text: /Yes, partially legit/
    assert_select_in html, "strong", text: "partially legit"
    assert_select_in html, "code", text: "main"
    assert_select_in html, "ul li", count: 2
    assert_select_in html, "a.console-markdown-link[href='https://github.com/paradigmxyz/centaur/issues/792']",
                     text: "https://github.com/paradigmxyz/centaur/issues/792"
  end

  test "restores every inline markdown link" do
    html = SkillMarkdown.render(<<~MARKDOWN)
      Compare [First resource](https://example.com/first),
      [Second resource](https://example.com/second), and [Third resource](https://example.com/third).
    MARKDOWN

    assert_select_in html, "a.console-markdown-link", count: 3
    assert_select_in html, "a.console-markdown-link[href='https://example.com/first']", text: "First resource"
    assert_select_in html, "a.console-markdown-link[href='https://example.com/second']", text: "Second resource"
    assert_select_in html, "a.console-markdown-link[href='https://example.com/third']", text: "Third resource"
    refute_match(/%%MDPH\d+%%/, html)
  end

  test "renders gfm tables with alignment" do
    html = SkillMarkdown.render(<<~MARKDOWN)
      Before the table.

      | Name | Count | Status |
      | :--- | ---: | :---: |
      | `api-rs` | 12 | **ok** |
      | console | 3 | pending |

      After the table.
    MARKDOWN

    assert_select_in html, "table thead tr th", count: 3
    assert_select_in html, "table tbody tr", count: 2
    assert_select_in html, "th.text-right", text: "Count"
    assert_select_in html, "th.text-center", text: "Status"
    assert_select_in html, "td.text-right", text: "12"
    assert_select_in html, "tbody code", text: "api-rs"
    assert_select_in html, "tbody strong", text: "ok"
    assert_select_in html, "p", text: "Before the table."
    assert_select_in html, "p", text: "After the table."
  end

  test "pads and truncates ragged table rows to the header width" do
    html = SkillMarkdown.render(<<~MARKDOWN)
      | a | b |
      | --- | --- |
      | only |
      | one | two | three |
    MARKDOWN

    assert_select_in html, "tbody tr", count: 2
    assert_select_in html, "tbody tr:first-child td", count: 2
    assert_select_in html, "tbody tr:last-child td", count: 2
    refute_includes html, "three"
  end

  test "escapes html inside table cells" do
    html = SkillMarkdown.render("| h |\n| --- |\n| <script>alert(1)</script> |")

    refute_includes html, "<script>"
    assert_select_in html, "tbody td", text: /alert\(1\)/
  end

  test "leaves pipe-prefixed lines without a separator as text" do
    html = SkillMarkdown.render("| not a table |\nplain line")

    assert_select_in html, "table", count: 0
    assert_select_in html, "p", count: 2
    assert_includes html, "not a table"
  end

  test "escapes unsafe html" do
    html = SkillMarkdown.render("<script>alert(1)</script> **safe**")

    refute_includes html, "<script>"
    assert_select_in html, "strong", text: "safe"
  end

  test "terminates on bare list and heading markers" do
    # A line that is only a block-start marker (no content) once spun the
    # markdown parser forever, pinning a worker. It must render and return.
    [ "- ", "* ", "+ ", "1. ", "# ", "## ", "hello\n- \nworld" ].each do |input|
      html = Timeout.timeout(3) { SkillMarkdown.render(input) }
      assert html.present?, "expected markdown for #{input.inspect}"
    end
  end

  private

  def assert_select_in(html, *args, &block)
    assert_select Nokogiri::HTML5.fragment(html), *args, &block
  end
end
