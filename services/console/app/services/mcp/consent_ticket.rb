require "securerandom"

module Mcp
  # Carries a validated MCP authorization request from GET /mcp/oauth/authorize
  # to the console SPA consent page and back to the decision endpoint.
  #
  # The ticket is a signed (tamper-proof, 10-minute) message holding the request
  # parameters, the user it was issued to, and a nonce. The nonce is also kept
  # in the console session and removed when a decision is made, so a ticket is
  # single-use, bound to the browser session that started it, and useless to a
  # different user. Ticket contents are not secret (they come from the client's
  # own authorize URL); the signature only stops the SPA from altering them.
  module ConsentTicket
    PURPOSE = :mcp_authorization_consent
    TTL = 10.minutes
    SESSION_KEY = :mcp_consent_nonces
    # A few concurrent consents (several MCP clients or tabs) per session; the
    # oldest pending nonce is dropped beyond this, keeping the cookie small.
    MAX_PENDING = 5

    module_function

    def issue(session, user:, request:)
      nonce = SecureRandom.urlsafe_base64(18)
      pending = Array(session[SESSION_KEY]).last(MAX_PENDING - 1)
      session[SESSION_KEY] = pending + [ nonce ]
      verifier.generate(
        { "user" => user.id, "nonce" => nonce, "params" => request.normalized_params },
        purpose: PURPOSE, expires_in: TTL
      )
    end

    # The authorization request for +ticket+ when it is authentic, unexpired,
    # issued to +user+, and still pending in +session+; otherwise nil. With
    # consume: true the nonce is removed first, so a replayed decision fails.
    def load(session, ticket, user:, consume: false)
      payload = verifier.verified(ticket.to_s, purpose: PURPOSE)
      return nil unless payload.is_a?(Hash) && user && payload["user"] == user.id

      pending = Array(session[SESSION_KEY])
      return nil unless pending.include?(payload["nonce"])

      session[SESSION_KEY] = pending - [ payload["nonce"] ] if consume
      AuthorizationRequest.from_params(payload["params"] || {})
    rescue ActiveSupport::MessageVerifier::InvalidSignature
      nil
    end

    def verifier
      Rails.application.message_verifier(PURPOSE)
    end
  end
end
