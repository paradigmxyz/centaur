module Api
  module V1
    # Admin self-descope ("view as operator") for the console session:
    #   POST   /api/v1/descope  pause admin permissions (acting admins only)
    #   DELETE /api/v1/descope  restore them (any descoped session)
    #
    # The flag lives in the cookie session, so bearer API keys cannot use it.
    class DescopesController < Api::BaseController
      allow_active_users only: :destroy
      before_action :require_session_auth!

      def create
        start_console_descope!
        render json: { data: { descoped: true } }
      end

      def destroy
        stop_console_descope! if descoped?
        render json: { data: { descoped: false } }
      end

      private

      def require_session_auth!
        return if session_authenticated?

        render_error(status: :unprocessable_entity, message: "descope requires a console session")
      end
    end
  end
end
