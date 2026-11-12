module Api
  module V1
    # Admin self-descope ("view as operator") for the console session:
    #   POST   /api/v1/descope  pause admin permissions (acting admins only)
    #   DELETE /api/v1/descope  restore them (any descoped session)
    #
    # The flag lives in the cookie session, so bearer API keys cannot use it
    # (403 details.reason=session_required).
    class DescopesController < Api::BaseController
      credential_policy :session

      allow_active_users only: :destroy

      def create
        start_console_descope!
        render json: { data: { descoped: true } }
      end

      def destroy
        stop_console_descope! if descoped?
        render json: { data: { descoped: false } }
      end
    end
  end
end
