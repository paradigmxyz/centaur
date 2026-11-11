module Api
  module V1
    # GET /api/v1/me
    #
    # Who the caller is and what the console SPA should show them. Reachable by
    # pending accounts (session only) so the SPA can render the holding page.
    # The CSRF token is returned only for session-authenticated requests; send
    # it back as X-CSRF-Token on every cookie-authenticated write. `flash`
    # carries (once) any notice/alert left by a redirect-based flow.
    class MeController < Api::BaseController
      allow_pending_users only: :show

      def show
        response.headers["Cache-Control"] = "no-store"
        render json: { data: payload }
      end

      private

      def payload
        user = current_user
        {
          user: {
            id: user.oid,
            email: user.email,
            name: user.name,
            status: user.status,
            admin: user.admin?
          },
          auth_method: auth_method.to_s,
          acting_admin: acting_admin?,
          descoped: descoped?,
          csrf_token: (form_authenticity_token if session_authenticated?),
          features: {
            api_clients: ApiClientsFeature.enabled?,
            password_login: ConsoleAuth.password_login_enabled?
          },
          nav: nav_sections(user),
          flash: consume_console_flash
        }
      end

      # Sections the SPA shell offers. Mirrors the server-side gates: admin
      # pages require acting_admin?, the rest any active account.
      def nav_sections(user)
        admin = acting_admin?
        active = user.active?
        {
          control: admin,
          principals: admin,
          roles: admin,
          secrets: admin,
          credentials: admin,
          oauth_apps: admin,
          users: admin,
          api_clients: admin && ApiClientsFeature.enabled?,
          settings: admin,
          workflows: admin,
          integrations: active,
          skills: active,
          scheduled_tasks: active
        }
      end
    end
  end
end
