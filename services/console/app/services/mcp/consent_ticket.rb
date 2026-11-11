require "securerandom"

module Mcp
  # Carries a validated MCP authorization request from GET /mcp/oauth/authorize
  # to the console SPA consent page and back to the decision endpoint.
  #
  # The ticket is a signed (tamper-proof, 10-minute) message holding the request
  # parameters, the user it was issued to, and a nonce. The nonce is bound
  # twice:
  # - to the browser session, through a short list of pending nonces in the
  #   console session (a ticket from another session or user is useless);
  # - to the server, through an McpOauthConsent row consumed by one atomic
  #   conditional UPDATE, so a ticket is decided at most once even when the
  #   original session cookie is replayed or decisions race.
  # Ticket contents are not secret (they come from the client's own authorize
  # URL); the signature only stops the browser from altering them.
  module ConsentTicket
    PURPOSE = :mcp_authorization_consent
    TTL = 10.minutes
    SESSION_KEY = :mcp_consent_nonces
    # A few concurrent consents (several MCP clients or tabs) per session; the
    # oldest pending nonce is dropped beyond this, keeping the cookie small.
    MAX_PENDING = 5

    module_function

    def issue(session, user:, request:)
      nonce = SecureRandom.urlsafe_base64(24)
      McpOauthConsent.prune!
      McpOauthConsent.create!(nonce_digest: McpOauthConsent.digest(nonce), user: user, expires_at: TTL.from_now)
      pending = Array(session[SESSION_KEY]).last(MAX_PENDING - 1)
      session[SESSION_KEY] = pending + [ nonce ]
      verifier.generate(
        { "user" => user.id, "nonce" => nonce, "params" => request.normalized_params },
        purpose: PURPOSE, expires_in: TTL
      )
    end

    # The authorization request for +ticket+ when it is authentic, unexpired,
    # issued to +user+, pending in +session+, and not yet decided; else nil.
    def load(session, ticket, user:)
      payload = pending_payload(session, ticket, user)
      return nil unless payload && McpOauthConsent.pending_for?(payload["nonce"], user)

      AuthorizationRequest.from_params(payload["params"] || {})
    end

    # Like load, but consumes the ticket: only the first caller for a ticket
    # gets the request back.
    def consume(session, ticket, user:)
      payload = pending_payload(session, ticket, user)
      return nil unless payload

      session[SESSION_KEY] = Array(session[SESSION_KEY]) - [ payload["nonce"] ]
      return nil unless McpOauthConsent.consume!(payload["nonce"], user)

      AuthorizationRequest.from_params(payload["params"] || {})
    end

    def pending_payload(session, ticket, user)
      payload = verifier.verified(ticket.to_s, purpose: PURPOSE)
      return nil unless payload.is_a?(Hash) && user && payload["user"] == user.id
      return nil unless Array(session[SESSION_KEY]).include?(payload["nonce"])

      payload
    rescue ActiveSupport::MessageVerifier::InvalidSignature
      nil
    end

    def verifier
      Rails.application.message_verifier(PURPOSE)
    end
  end
end
