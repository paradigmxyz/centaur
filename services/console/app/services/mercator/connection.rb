module Mercator
  # Centaur's control database is deployment-scoped. One shared connection per
  # deployment, not one per console user or Slack workspace.
  class Connection
    SLUG = "mercator"

    def self.app
      OauthApp.find_by(slug: SLUG, provider: "mercator")
    end

    def self.credential
      app&.broker_credentials&.first
    end

    def self.manual_configuration?
      return true if BrokerCredential.where(token_endpoint: Oauth::Providers::Mercator.new.token_endpoint)
        .where.not(id: app&.broker_credentials&.select(:id) || []).exists?
      StaticSecret.joins(:rules).where(request_rules: { host: "mercator.sh" })
        .where.not(id: credential&.static_secret&.id || []).exists?
    end

    def self.prepare!(user:, redirect_uri:, client: ConnectionClient.new)
      raise ActiveRecord::RecordNotFound unless user.active? && user.admin?
      OauthApp.transaction do
        # Serialize first-time registration before there is an OauthApp to lock.
        OauthApp.connection.execute("SELECT pg_advisory_xact_lock(1835364963, 1)")
        existing = OauthApp.find_by(slug: SLUG)
        if existing
          unless existing.provider == "mercator" && existing.enabled?
            raise Broker::ExchangeError.new("Mercator app unavailable", stage: "oauth", code: "app_unavailable")
          end
          return existing
        end
        if manual_configuration?
          raise Broker::ExchangeError.new("Existing Mercator credential requires review", stage: "oauth", code: "existing_credential")
        end
        registration = client.register(redirect_uri)
        OauthApp.create!(registration.merge(
          slug: SLUG, provider: "mercator", allowed_scopes: [ Oauth::Providers::Mercator::SCOPE ],
          description: "Shared Mercator wallet", created_by: user, enabled: true
        ))
      end
    end
  end
end
