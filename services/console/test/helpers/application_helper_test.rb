require "test_helper"

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

  test "local_time renders a placeholder for nil" do
    assert_select_in local_time(nil), "span", text: "—"
  end

  private

  def assert_select_in(html, *args, &block)
    assert_select Nokogiri::HTML5.fragment(html), *args, &block
  end
end
