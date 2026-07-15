require "json"
require "net/http"
require "uri"

# Resolves a Console requester's GitHub handle from the GitHub account they
# connected themselves. Older credentials predate the github_login label, so
# they fall back to GitHub's authenticated-user endpoint.
class GithubRequesterIdentity
  Result = Data.define(:handle, :source, :reason)
  USER_ENDPOINT = "https://api.github.com/user".freeze
  LOGIN_LABEL = "github_login".freeze
  LOGIN_PATTERN = /\A[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?\z/

  class << self
    attr_accessor :github_api_http

    def resolve(user:)
      return unavailable("Console user is unavailable") unless user

      credentials = BrokerCredential
        .joins(:oauth_app)
        .where(created_by: user, oauth_apps: { provider: Oauth::Providers::Github::KEY })
        .order(updated_at: :desc)

      return unavailable("no connected GitHub account found") if credentials.empty?

      credentials.each do |credential|
        login = normalized_login(credential.labels&.[](LOGIN_LABEL))
        return verified(login, "connected GitHub account") if login

        login = login_from_api(credential.access_token)
        return verified(login, "connected GitHub account (GitHub API)") if login
      end

      unavailable("connected GitHub account did not return a valid login")
    end

    private

    def verified(login, source)
      Result.new(handle: "@#{login}", source: source, reason: nil)
    end

    def unavailable(reason)
      Result.new(handle: nil, source: nil, reason: reason)
    end

    def normalized_login(value)
      login = value.to_s.strip.delete_prefix("@")
      login if login.match?(LOGIN_PATTERN)
    end

    def login_from_api(access_token)
      response = github_api(access_token)
      normalized_login(response["login"]) if response.is_a?(Hash)
    rescue StandardError => e
      Rails.logger.warn("console_github_requester_identity_lookup_failed error=#{e.class}")
      nil
    end

    def github_api(access_token)
      return nil if access_token.blank?
      return github_api_http.call(url: USER_ENDPOINT, access_token: access_token) if github_api_http

      uri = URI.parse(USER_ENDPOINT)
      request = Net::HTTP::Get.new(uri)
      request["Accept"] = "application/vnd.github+json"
      request["Authorization"] = "Bearer #{access_token}"
      request["X-GitHub-Api-Version"] = "2022-11-28"
      request["User-Agent"] = "centaur-console"
      response = Net::HTTP.start(
        uri.host, uri.port, use_ssl: true, open_timeout: 2, read_timeout: 5
      ) { |http| http.request(request) }
      return nil unless response.code.to_i / 100 == 2

      JSON.parse(response.body.to_s)
    end
  end
end
