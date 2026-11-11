# Redirect-only form endpoints for console password login and logout. The login
# and pending pages live in the SPA (web/src/features/auth-pages), which signs
# in through POST /api/v1/session and out through DELETE /api/v1/session.
#
# POST /login is kept for form posts from older pages and request tests until
# the legacy UI is gone; it shares the sign-in logic (ConsoleSignIn) and the
# CSRF protection of every form post, and reports failures through the flash
# on the SPA login page. DELETE /logout serves the legacy account menu.
class SessionsController < ApplicationController
  skip_before_action :require_login, only: :create
  # Pending users must still be able to sign out (and sign in as someone else).
  skip_before_action :require_active_account

  def create
    unless ConsoleAuth.password_login_enabled?
      return redirect_to login_path, alert: "Email and password sign in is disabled."
    end

    user = User.find_by(email: params[:email].to_s.strip.downcase)
    return redirect_to login_path, alert: "Invalid email or password." unless user&.authenticate(params[:password])

    sign_in_console_user(user)
  end

  def destroy
    reset_session
    redirect_to login_path, notice: "Signed out."
  end
end
