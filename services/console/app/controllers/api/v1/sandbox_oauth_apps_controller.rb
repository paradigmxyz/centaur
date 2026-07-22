module Api
  module V1
    class SandboxOauthAppsController < ActionController::API
      include ApiRequestSupport

      before_action :authenticate_sandbox_jwt!

      def index
        apps = OauthApp.where(enabled: true).order(:slug, :id)
        render json: { data: apps.map { |app| app_payload(app) } }
      end

      private

      def authenticate_sandbox_jwt!
        token = bearer_token
        if token.blank?
          return render_error(status: :unauthorized, message: "invalid or missing sandbox token")
        end

        # This endpoint only needs to prove the caller has the same signed
        # sandbox entitlement used by /sandbox/permissions. The listed start URLs
        # are public OAuth entrypoints, so no proxy/principal claim check is
        # needed here.
        SandboxEntitlements::Jwt.decode(token)
      rescue CentaurJwt::Hs256::VerificationError
        render_error(status: :unauthorized, message: "invalid or missing sandbox token")
      end

      def app_payload(app)
        {
          id: app.oid,
          slug: app.slug,
          description: app.description,
          labels: app.labels,
          provider: app.provider,
          allowed_scopes: app.allowed_scopes,
          start_url: oauth_start_url_for(app)
        }
      end

      def oauth_start_url_for(app)
        URI.join(public_base_url, "/oauth/#{app.slug}/start").to_s
      end

      def public_base_url
        ConsoleEnv["PUBLIC_URL"].presence || request.base_url
      end
    end
  end
end
