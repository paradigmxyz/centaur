module Console
  # Admin "view as operator" support: an admin can temporarily pause their own
  # admin permissions to see the console as a regular operator would. The flag
  # lives in the cookie session; acting_admin? (the check every admin gate uses)
  # is false while it's set, and ApplicationController drops it automatically if
  # the user is no longer an admin.
  #
  # create is admin-gated. destroy is deliberately not: while descoped, the user
  # fails require_admin, but they must always be able to restore themselves.
  class DescopesController < ApplicationController
    before_action :require_admin, only: :create

    def create
      session[:descoped] = true
      Rails.logger.info("console_descope_started admin=#{current_user.email}")
      # No flash: the persistent descope banner already announces the state.
      redirect_to console_threads_path
    end

    def destroy
      return redirect_to default_console_landing_path unless descoped?

      session.delete(:descoped)
      Rails.logger.info("console_descope_stopped admin=#{current_user.email}")
      redirect_to console_principals_path, notice: "Admin permissions restored."
    end
  end
end
