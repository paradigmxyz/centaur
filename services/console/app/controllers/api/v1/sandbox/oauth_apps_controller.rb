module Api
  module V1
    module Sandbox
      class OauthAppsController < ActionController::API
        include ApiRequestSupport
        include OauthAppUrls

        before_action :authenticate_sandbox_jwt!

        def index
          apps = OauthApp.where(enabled: true, shared: false).order(:slug, :id)
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
            start_url: oauth_start_url(app.slug)
          }
        end
      end
    end
  end
end
