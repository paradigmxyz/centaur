# The server-rendered sign-in and OAuth pages load only same-origin static
# files under /assets/ (plus /icon.svg), so they work behind proxies that only
# expose those paths, and every referenced file exists.
module PageAssetsHelper
  def assert_page_assets_under_assets_prefix
    doc = Nokogiri::HTML(response.body)
    urls = doc.css("link[href]").map { |node| node["href"] } + doc.css("[src]").map { |node| node["src"] }
    local = urls.map { |url| URI.parse(url) }.reject(&:host).map(&:path).reject { |path| path.start_with?("/oauth/", "/mcp/") }
    assert local.any?, "expected the page to reference static assets"
    local.each do |path|
      assert path == "/icon.svg" || path.start_with?("/assets/"), "#{path} must live under /assets/ (or be /icon.svg)"
      assert File.file?(Rails.public_path.join(path.delete_prefix("/"))), "#{path} does not exist in public/"
    end
  end
end

ActiveSupport.on_load(:action_dispatch_integration_test) { include PageAssetsHelper }
