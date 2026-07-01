require "test_helper"
require "timeout"

class ApplicationHelperTest < ActionView::TestCase
  test "truncate_middle leaves short values unchanged" do
    assert_equal "short", truncate_middle("short")
    assert_equal "", truncate_middle(nil)
  end

  test "truncate_middle keeps the head and tail around a center ellipsis" do
    out = truncate_middle("salesforce-marketing-cloud-rest-api", max: 24)
    assert_equal 24, out.length
    assert out.start_with?("salesforce")
    assert out.end_with?("rest-api")
    assert_includes out, "…"
  end

  test "truncate_middle respects a custom max and omission" do
    assert_equal "abc...xyz", truncate_middle("abcdefghijklmnopqrstuvwxyz", max: 9, omission: "...")
  end

  test "local_time renders a Stimulus-localized time element with an ISO fallback" do
    html = local_time(Time.utc(2026, 6, 4, 18, 30, 0))
    assert_select_in html, "time[data-controller=localtime]" do
      assert_select "time[datetime=?]", "2026-06-04T18:30:00Z"
      assert_select "time[data-localtime-relative-value=false]"
    end
    # The visible text is the ISO fallback, shown until the controller connects.
    assert_includes html, ">2026-06-04T18:30:00Z<"
  end

  test "local_time marks relative timestamps for the time-ago controller" do
    html = local_time(Time.utc(2026, 6, 4, 18, 30, 0), relative: true)
    assert_select_in html, "time[data-localtime-relative-value=true]"
  end

  test "local_time can request compact relative formatting" do
    html = local_time(Time.utc(2026, 6, 4, 18, 30, 0), relative: true, format: :compact)

    assert_select_in html, "time[data-localtime-relative-value=true]"
    assert_select_in html, "time[data-localtime-format-value=compact]"
  end

  test "local_time renders a placeholder for nil" do
    assert_select_in local_time(nil), "span", text: "—"
  end

  test "console_markdown renders common github-flavored markdown" do
    html = console_markdown(<<~MARKDOWN)
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

  test "console_markdown escapes unsafe html" do
    html = console_markdown("<script>alert(1)</script> **safe**")

    refute_includes html, "<script>"
    assert_select_in html, "strong", text: "safe"
  end

  test "console_markdown terminates on bare list and heading markers" do
    # A line that is only a block-start marker (no content) once spun the
    # markdown parser forever, pinning a worker. It must render and return.
    [ "- ", "* ", "+ ", "1. ", "# ", "## ", "hello\n- \nworld" ].each do |input|
      html = Timeout.timeout(3) { console_markdown(input) }
      assert html.present?, "expected markdown for #{input.inspect}"
    end
  end

  test "console_icon renders theme toggle icons" do
    assert_select_in console_icon("sun"), "svg path[d*='M12 3v2.25']"
    assert_select_in console_icon("moon"), "svg path[d*='21.752 15.002']"
  end

  private

  def assert_select_in(html, *args, &block)
    assert_select Nokogiri::HTML5.fragment(html), *args, &block
  end
end
