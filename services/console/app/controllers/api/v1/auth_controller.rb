module Api
  module V1
    # GET /api/v1/auth/config -- public bootstrap for the SPA's signed-out pages
    # (login, OAuth consent result). Needs no session and no API key.
    #
    # Returns the sign-in methods, a CSRF token bound to the (possibly new)
    # session for POST /api/v1/session, the one-shot flash a redirect-based
    # flow left behind (SSO errors, sign-out notice, OAuth consent result), and,
    # when the browser is already signed in to an active account, where the
    # login page should send it instead.
    #
    # ?return_to= (the page a signed-out user was trying to reach) is
    # remembered for the post-login redirect when it is an allowlisted
    # same-origin path (ConsoleReturnPath); anything else is ignored.
    class AuthController < Api::BaseController
      include ConsoleSignIn

      skip_before_action :authenticate_request!, :require_active_account!,
                         :verify_session_csrf_token!, :require_admin!

      def show
        response.headers["Cache-Control"] = "no-store"
        user = signed_in_user
        remember_console_return_path(params[:return_to]) if user.nil? && params[:return_to].present?
        oauth_result = request.flash[:oauth_result]
        flash = consume_console_flash

        render json: {
          data: {
            providers: ConsoleAuth.providers.map { |key| provider_payload(key) },
            password_login: ConsoleAuth.password_login_enabled?,
            csrf_token: form_authenticity_token,
            signed_in: user.present?,
            redirect_to: (redirect_for(user) if user&.active?),
            flash: flash,
            oauth_result: (oauth_result.slice("kind", "message", "retry_path") if oauth_result.is_a?(Hash))
          }
        }
      end

      private

      # The session user, unless the account is disabled (signed out here, as
      # every console gate does).
      def signed_in_user
        user = console_session_user
        return user unless user&.disabled?

        reset_session
        nil
      end

      def redirect_for(user)
        ConsoleReturnPath.safe(params[:return_to]) || console_landing_path_for(user)
      end

      def provider_payload(key)
        { key: key, label: key.capitalize, start_path: "/auth/#{key}/start" }
      end
    end
  end
end
