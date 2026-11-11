module Api
  module V1
    # The MCP OAuth consent page (SPA /mcp/oauth/consent) reads and decides a
    # consent ticket issued by GET /mcp/oauth/authorize (Mcp::ConsentTicket).
    #
    # GET  /api/v1/mcp/authorization?request=<ticket>   what is being authorized
    # POST /api/v1/mcp/authorization/decision            approve or deny
    #      { "data": { "request": "<ticket>", "decision": "approve" | "deny" } }
    #
    # Console session only (bearer callers get 403 `session_required`): consent
    # is a browser interaction bound to the session that started it, and the
    # decision is CSRF-protected like every cookie write. Any active account may
    # consent for itself. The decision consumes the ticket (a replay fails) and
    # revalidates the whole request server-side; the response's redirect_to is
    # always the client's registered redirect_uri, never a browser-chosen URL.
    class McpAuthorizationsController < Api::BaseController
      DECISIONS = %w[approve deny].freeze
      INVALID_TICKET_MESSAGE =
        "This authorization request is invalid or has expired. Start again from your MCP client.".freeze

      allow_active_users only: %i[show decide]
      before_action :require_console_session!

      def show
        authorization = Mcp::ConsentTicket.load(session, ticket_param, user: current_user)
        return render_invalid_ticket unless authorization
        return render_invalid_request(authorization) unless authorization.valid?

        render json: { data: details_payload(authorization) }
      end

      def decide
        decision = decision_params[:decision].to_s
        unless DECISIONS.include?(decision)
          return render_error(status: :unprocessable_entity, message: "decision must be approve or deny.",
                              details: { decision: [ "must be approve or deny" ] })
        end

        authorization = Mcp::ConsentTicket.load(session, decision_params[:request], user: current_user, consume: true)
        return render_invalid_ticket unless authorization
        return render_invalid_request(authorization) unless authorization.valid?

        redirect_url = decision == "approve" ? authorization.issue_code!(current_user) : authorization.denied_redirect_url
        render json: { data: { decision: decision, redirect_to: redirect_url } }
      rescue ActiveRecord::RecordInvalid => e
        Rails.logger.error { "mcp authorization code issue failed: #{e.record.errors.full_messages.to_sentence}" }
        render json: {
          data: { decision: decision, redirect_to: authorization.error_redirect_url(:server_error, "The authorization code could not be issued.") }
        }
      end

      private

      def require_console_session!
        return if session_authenticated?

        render_error(status: :forbidden, message: "MCP authorization requires a console session",
                     details: { reason: "session_required" })
      end

      def ticket_param
        params[:request].to_s
      end

      def decision_params
        params.permit(data: %i[request decision])[:data] || {}
      end

      def details_payload(authorization)
        {
          client: { id: authorization.client.public_client_id, name: authorization.client.name },
          redirect_host: authorization.redirect_host,
          resource: authorization.resource,
          scopes: authorization.scopes,
          user: { email: current_user.email }
        }
      end

      def render_invalid_ticket
        render_error(status: :not_found, message: INVALID_TICKET_MESSAGE, details: { reason: "invalid_consent_request" })
      end

      # The client or its registration changed after the ticket was issued.
      # The browser is told where the client expects the error (its registered
      # redirect_uri) when that is still trusted; nothing is issued.
      def render_invalid_request(authorization)
        render_error(status: :unprocessable_entity, message: authorization.error_description,
                     details: { reason: "invalid_authorization_request", error: authorization.error.to_s,
                                redirect_to: authorization.error_redirect_url })
      end
    end
  end
end
