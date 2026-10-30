# The Broker namespace holds the OAuth consent flow's authorization-code exchange
# and its error types.
#
# SECURITY: every code path under Broker handles refresh tokens and access tokens.
# Logging is restricted to credential ids, OAuth error codes, and timestamps --
# never the tokens themselves or raw token-endpoint bodies.
module Broker
  class Error < StandardError; end

  # Raised by AuthorizationCodeClient (and the provider identity extraction) when
  # a one-shot consent-flow code exchange fails. It carries no `retryable` flag: a
  # consent flow is synchronous and any failure surfaces to the user as an error
  # result page rather than entering a backoff loop. `code`
  # is an OAuth error code or a flow-specific marker (e.g. "missing_refresh_token",
  # "id_token_aud_mismatch").
  class ExchangeError < Error
    STAGES = %w[network http oauth parse].freeze

    attr_reader :stage, :code, :status

    def initialize(message, stage:, code: nil, status: nil)
      super(message)
      @stage = stage
      @code = code
      @status = status
    end

    # The label shown on the flow's error result page: the OAuth/flow error code
    # when present, else the stage.
    def reason = code.presence || stage
  end
end
