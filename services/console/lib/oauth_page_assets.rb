require "digest"

# URLs for the static files behind the server-rendered OAuth pages
# (layouts/oauth.html.erb), served from public/oauth-page without an asset
# pipeline. public/ is served with a long cache lifetime, so each URL carries a
# digest of the file's contents: a deploy that changes a file changes its URL.
module OauthPageAssets
  DIRECTORY = "oauth-page".freeze

  module_function

  def path(name)
    digests[name] ||= Digest::SHA256.file(Rails.public_path.join(DIRECTORY, name)).hexdigest.first(12)
    "/#{DIRECTORY}/#{name}?v=#{digests[name]}"
  end

  def digests
    @digests ||= Concurrent::Map.new
  end
end
