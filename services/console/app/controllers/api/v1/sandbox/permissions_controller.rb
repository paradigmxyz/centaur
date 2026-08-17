module Api
  module V1
    module Sandbox
      class PermissionsController < Api::SandboxBaseController
        def show
          principal = current_proxy.principal
          unless principal
            return render_error(status: :unauthorized, message: "sandbox token is no longer assigned")
          end

          # Redacting the cached snapshot stores the unredacted config but skips
          # the expensive per-request grant rebuild, under the same freshness
          # model the proxy sync path accepts.
          snapshot = PrincipalSyncConfigSnapshot.fetch_for(principal)
          permissions = Principal.redact_live_secrets(snapshot.config)
          body = {
            data: {
              sandbox_id: sandbox_claims.fetch("sandbox_id"),
              proxy_id: current_proxy.oid,
              principal_id: principal.oid,
              principal: principal_payload(principal),
              capabilities: capabilities_payload(principal),
              slack_channel_permissions: principal.effective_slack_channel_permissions_payload,
              oauth_credentials: oauth_credentials_payload(principal),
              permissions: permissions
            }
          }.to_json

          response.headers["ETag"] = %("#{Digest::SHA256.hexdigest(body)}")
          response.headers["Cache-Control"] = "no-store"
          render json: body
        end

        private

        def principal_payload(principal)
          {
            id: principal.oid,
            foreign_id: principal.foreign_id,
            name: principal.name
          }
        end

        def capabilities_payload(principal)
          {
            sandbox_repo_cache: principal.sandbox_repo_cache,
            sandbox_observability_enabled: principal.sandbox_observability_enabled,
            sandbox_sessions_read_enabled: principal.sandbox_sessions_read_enabled,
            sandbox_workflows_read_enabled: principal.sandbox_workflows_read_enabled,
            sandbox_workflows_write_enabled: principal.sandbox_workflows_write_enabled
          }
        end

        def oauth_credentials_payload(principal)
          principal.granted_static_secrets
            .includes(broker_credential: :oauth_app)
            .filter_map(&:broker_credential)
            .select(&:oauth_app)
            .sort_by { |credential| [ credential.oauth_app.slug, credential.provider_email.to_s, credential.id ] }
            .map do |credential|
              {
                id: credential.oid,
                oauth_app_id: credential.oauth_app.oid,
                slug: credential.oauth_app.slug,
                provider: credential.oauth_app.provider,
                provider_email: credential.provider_email,
                provider_subject: credential.provider_subject,
                status: credential.status,
                scopes: credential.scopes
              }
            end
        end
      end
    end
  end
end
