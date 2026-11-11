# Cookie-backed console session state shared by the HTML controllers
# (ApplicationController) and the JSON API (Api::BaseController), so both resolve
# the signed-in operator, the admin self-descope flag, and the acting-admin check
# the same way.
module ConsoleSession
  extend ActiveSupport::Concern

  private

  # The User whose id is stored in the console session cookie, or nil. Callers
  # still check the account status; this only resolves identity.
  def console_session_user
    @console_session_user ||= User.find_by(id: session[:user_id]) if session[:user_id]
  end

  # Whether this admin has temporarily descoped themselves to operator
  # permissions ("view as operator"). Self-healing: the flag is dropped if the
  # user is no longer an admin, so it can never outlive the privileges it pauses.
  def console_session_descoped?
    return false unless session[:descoped]
    return true if console_session_user&.admin?

    session.delete(:descoped)
    false
  end

  def start_console_descope!
    session[:descoped] = true
    Rails.logger.info("console_descope_started admin=#{console_session_user.email}")
  end

  def stop_console_descope!
    session.delete(:descoped)
    Rails.logger.info("console_descope_stopped admin=#{console_session_user.email}")
  end
end
