module Api
  module V1
    # DELETE /api/v1/session signs the console session out. Bearer callers have
    # no session to end, so the request is a no-op for them. Pending accounts
    # may sign out too.
    class SessionsController < Api::BaseController
      allow_pending_users only: :destroy

      def destroy
        reset_session if session_authenticated?
        head :no_content
      end
    end
  end
end
