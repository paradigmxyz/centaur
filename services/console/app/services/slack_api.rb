require "json"

module SlackApi
  DEFAULT_MAX_RATE_LIMIT_WAIT_SECONDS = 5.minutes.to_i
  DEFAULT_TRANSIENT_RETRY_AFTER_SECONDS = 30
  TRANSIENT_ERRORS = %w[fatal_error internal_error].freeze

  class Error < StandardError
    attr_reader :code

    def initialize(message = nil, code: nil)
      @code = code
      super(message)
    end
  end

  class RetryableError < Error
    attr_reader :retry_after

    def initialize(message = nil, retry_after:, code: nil)
      @retry_after = retry_after
      super(message || "Slack API request is retryable after #{retry_after} seconds", code: code)
    end
  end

  class RateLimitedError < RetryableError; end
  class TransientError < RetryableError; end

  module_function

  def parse_response!(response, operation: nil,
                      max_rate_limit_wait: DEFAULT_MAX_RATE_LIMIT_WAIT_SECONDS,
                      transient_retry_after: DEFAULT_TRANSIENT_RETRY_AFTER_SECONDS)
    context = operation.present? ? " for #{operation}" : ""
    if response.status == 429
      retry_after = Float(response["retry-after"], exception: false)
      retry_after = 1 unless retry_after&.positive?
      raise RateLimitedError.new(
        "Slack API rate limited#{context}.",
        retry_after: [ retry_after, max_rate_limit_wait ].min,
        code: "ratelimited"
      )
    end

    body = response.json
    raise Error, "Slack API returned HTTP #{response.status}#{context}." unless response.success?
    if TRANSIENT_ERRORS.include?(body["error"])
      raise TransientError.new(
        "Slack API returned #{body['error']}#{context}.",
        retry_after: transient_retry_after,
        code: body["error"]
      )
    end
    unless body["ok"] == true
      error_code = body["error"]
      raise Error.new(
        "Slack API returned #{error_code || 'an unknown error'}#{context}.",
        code: error_code
      )
    end

    body
  rescue JSON::ParserError
    raise Error, "Slack API response#{context} was not JSON."
  end
end
