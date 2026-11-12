module Api
  module V1
    # DELETE /api/v1/session signs the console session out. Password sign-in is
    # the server-rendered POST /login form (SessionsController).
    #
    # Session only (a bearer key gets 403 session_required). Pending accounts
    # may sign out too.
    class SessionsController < Api::BaseController
      accepts_credentials :session

      allow_pending_users only: :destroy

      def destroy
        reset_session
        request.flash[:notice] = "Signed out."
        head :no_content
      end
    end
  end
end
