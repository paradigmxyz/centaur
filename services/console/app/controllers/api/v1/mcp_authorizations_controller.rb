module Api
  module V1
    # GET /api/v1/mcp/authorization?request=<ticket>
    #
    # What the MCP consent page (SPA /mcp/oauth/consent) shows: the client,
    # redirect host, resource, scopes, and account for a consent ticket issued
    # by GET /mcp/oauth/authorize (Mcp::ConsentTicket), plus the CSRF token for
    # the decision form. The decision itself is a classic form post to
    # POST /mcp/oauth/consent (Mcp::OauthController#decide), answered with a
    # server redirect, so no client redirect URL or authorization code ever
    # reaches page JavaScript.
    #
    # Console session only (bearer callers get 403 `session_required`). Any
    # active account may consent for itself.
    class McpAuthorizationsController < Api::BaseController
      INVALID_TICKET_MESSAGE =
        "This authorization request is invalid or has expired. Start again from your MCP client.".freeze

      allow_active_users only: :show
      before_action :require_console_session!

      def show
        response.headers["Cache-Control"] = "no-store"
        authorization = Mcp::ConsentTicket.load(session, params[:request].to_s, user: current_user)
        return render_invalid_ticket unless authorization
        return render_invalid_request(authorization) unless authorization.valid?

        render json: { data: details_payload(authorization) }
      end

      private

      def require_console_session!
        return if session_authenticated?

        render_error(status: :forbidden, message: "MCP authorization requires a console session",
                     details: { reason: "session_required" })
      end

      def details_payload(authorization)
        {
          client: { id: authorization.client.public_client_id, name: authorization.client.name },
          redirect_host: authorization.redirect_host,
          resource: authorization.resource,
          scopes: authorization.scopes,
          user: { email: current_user.email },
          decision_path: Mcp::OauthController::CONSENT_PATH,
          csrf_token: form_authenticity_token
        }
      end

      def render_invalid_ticket
        render_error(status: :not_found, message: INVALID_TICKET_MESSAGE, details: { reason: "invalid_consent_request" })
      end

      # The client or its registration changed after the ticket was issued.
      def render_invalid_request(authorization)
        render_error(status: :unprocessable_entity, message: authorization.error_description,
                     details: { reason: "invalid_authorization_request", error: authorization.error.to_s })
      end
    end
  end
end
