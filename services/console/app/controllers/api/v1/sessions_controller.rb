module Api
  module V1
    # POST /api/v1/session signs a browser in with email + password (the SPA
    # login page). DELETE /api/v1/session signs the console session out.
    #
    # Sign-in needs no session or API key, but it is CSRF-protected like every
    # cookie write (login CSRF): the token comes from GET /api/v1/auth/config.
    # No registration, password reset, or rate limiting: deployments that expose
    # the console publicly should disable this break-glass path with
    # CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED=false.
    #
    # Bearer callers have no session to end, so DELETE is a no-op for them.
    # Pending accounts may sign out too.
    class SessionsController < Api::BaseController
      include ConsoleSignIn
      # Bodies use the {data: {...}} envelope; no params wrapping.
      wrap_parameters false

      skip_before_action :authenticate_request!, :require_active_account!,
                         :verify_session_csrf_token!, :require_admin!, only: :create
      before_action :verify_login_csrf_token!, only: :create
      allow_pending_users only: :destroy

      def create
        unless ConsoleAuth.password_login_enabled?
          return render_error(status: :not_found, message: "Email and password sign in is disabled.",
                              details: { reason: "password_login_disabled" })
        end

        credentials = params.permit(data: %i[email password])[:data] || {}
        user = User.find_by(email: credentials[:email].to_s.strip.downcase)
        unless user&.authenticate(credentials[:password].to_s)
          return render_error(status: :unprocessable_entity, message: "Invalid email or password.",
                              details: { reason: "invalid_credentials" })
        end
        if user.disabled?
          return render_error(status: :unprocessable_entity, message: ConsoleSignIn::DISABLED_MESSAGE,
                              details: { reason: "account_disabled" })
        end

        redirect_to = establish_console_session!(user)
        render json: { data: { status: user.status, redirect_to: redirect_to } }
      end

      def destroy
        if session_authenticated?
          reset_session
          request.flash[:notice] = "Signed out."
        end
        head :no_content
      end

      private

      def verify_login_csrf_token!
        return if valid_request_origin? && any_authenticity_token_valid?

        render_error(status: :forbidden, message: "invalid or missing CSRF token", details: { reason: "csrf" })
      end
    end
  end
end
