# Resolves a Console requester's GitHub handle from the GitHub account they
# connected themselves. Identity enrichment stores the login ahead of the chat
# request, so this resolver remains a database-only lookup.
class GithubRequesterIdentity
  Result = Data.define(:handle, :source, :reason)
  LOGIN_LABEL = "github_login".freeze
  LOGIN_PATTERN = /\A[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?\z/

  class << self
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
      end

      unavailable("connected GitHub account is awaiting login enrichment")
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
  end
end
