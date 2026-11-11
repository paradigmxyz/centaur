# Establishes the console cookie session after a successful login (password via
# POST /api/v1/session, or SSO via /auth/:provider/callback) and decides where
# the browser goes next. Shared by ApplicationController and Api::BaseController.
#
# The caller must already have rejected disabled accounts. The session is reset
# (session fixation) but the remembered return path survives it; only
# allowlisted same-origin paths (ConsoleReturnPath) are honoured.
module ConsoleSignIn
  extend ActiveSupport::Concern

  DISABLED_MESSAGE = "Your account is disabled.".freeze
  PENDING_PATH = "/pending".freeze

  private

  # Signs +user+ in and returns the path to send the browser to. Leaves a
  # one-shot notice in the flash for the next page.
  def establish_console_session!(user)
    raise ArgumentError, "disabled users cannot sign in" if user.disabled?

    return_to = ConsoleReturnPath.safe(session[:return_to])
    reset_session
    session[:user_id] = user.id
    if user.active?
      request.flash[:notice] = "Signed in as #{user.email}."
      return_to || console_landing_path_for(user)
    else
      # Kept so the user lands there once an admin approves the account.
      session[:return_to] = return_to if return_to
      request.flash[:notice] = "Your account is awaiting approval."
      PENDING_PATH
    end
  end

  # Remembers where a signed-out browser was going, if it is an allowed path.
  def remember_console_return_path(path)
    safe = ConsoleReturnPath.safe(path)
    session[:return_to] = safe if safe
  end

  # A freshly signed-in session is never descoped, so this is the user's own
  # admin flag. Mirrors defaultLandingPath in web/src/app/session.ts.
  def console_landing_path_for(user)
    user.admin? ? "/console/principals" : "/console/integrations"
  end
end
