# Cookie-backed console session state shared by the HTML controllers
# (ApplicationController) and the JSON API (Api::BaseController), so both resolve
# the signed-in operator the same way.
module ConsoleSession
  extend ActiveSupport::Concern

  private

  # The User whose id is stored in the console session cookie, or nil. Callers
  # still check the account status; this only resolves identity.
  def console_session_user
    @console_session_user ||= User.find_by(id: session[:user_id]) if session[:user_id]
  end
end
