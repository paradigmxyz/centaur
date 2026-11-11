module Api
  module V1
    # DELETE /api/v1/session signs the console session out (the SPA's sign-out
    # button). Password sign-in is the server-rendered POST /login form
    # (SessionsController).
    #
    # Bearer callers have no session to end, so DELETE is a no-op for them.
    # Pending accounts may sign out too.
    class SessionsController < Api::BaseController
      allow_pending_users only: :destroy

      def destroy
        if session_authenticated?
          reset_session
          request.flash[:notice] = "Signed out."
        end
        head :no_content
      end
    end
  end
end
