# Quick session-cookie login for the operator console. Authenticates an existing
# User (has_secure_password) by email + password and stores their id in the
# session. No registration, password reset, or rate limiting: deployments that
# expose the console publicly should disable this break-glass path with
# CENTAUR_CONSOLE_PASSWORD_LOGIN_ENABLED=false.
#
# Intentionally server-rendered (layouts/oauth, no JavaScript): sign-in is a
# step of the MCP OAuth flow. Keep the form contract (POST /login with
# email/password and the Rails authenticity token, DELETE /logout) stable so
# pods on either version accept each other's forms during a rolling deploy.
class SessionsController < ApplicationController
  layout "oauth"
  include ServerRenderedPagePolicy

  # The login form must be reachable while signed out, so it opts out of the
  # app-wide require_login gate. (logout keeps the gate: it's a no-op when
  # there's no session.)
  skip_before_action :require_login, only: %i[new create]
  # None of these enforce approval: the login form is anonymous, and pending users
  # must still reach the holding page and be able to sign out.
  skip_before_action :require_active_account

  # GET /login. A signed-in active user goes to the allowlisted ?return_to= or
  # their landing page. A signed-out browser's allowlisted ?return_to= (the SPA
  # sends it on a 401) is remembered for the post-login redirect.
  def new
    return redirect_to(safe_return_to || default_console_landing_path) if current_user&.active?

    remember_console_return_path(params[:return_to]) if current_user.nil?
  end

  # GET /pending: holding page for a signed-in but not-yet-approved user.
  # Active users have no reason to be here, so send them to the console.
  def pending
    return redirect_to default_console_landing_path if current_user.active?

    @email = current_user.email
  end

  def create
    unless password_login_enabled?
      flash.now[:alert] = "Email and password sign in is disabled."
      return render :new, status: :not_found
    end

    user = User.find_by(email: params[:email].to_s.strip.downcase)
    unless user&.authenticate(params[:password].to_s)
      flash.now[:alert] = "Invalid email or password."
      return render :new, status: :unprocessable_entity
    end

    if user.disabled?
      flash.now[:alert] = ConsoleSignIn::DISABLED_MESSAGE
      return render :new, status: :unprocessable_entity
    end

    redirect_to establish_console_session!(user)
  end

  def destroy
    reset_session
    redirect_to login_path, notice: "Signed out."
  end

  private

  helper_method :password_login_enabled?

  def password_login_enabled?
    ConsoleAuth.password_login_enabled?
  end

  def safe_return_to
    ConsoleReturnPath.safe(params[:return_to].presence || params[:next].presence)
  end
end
