# Optional provider-specific details for an OAuth app's detail page (today the
# Mercator shared-wallet panel). Only a shared app has one account to describe:
# its first minted credential. Details are fetched only while that credential
# is live with an unexpired access token -- never refreshing it -- over tight
# timeouts, and any failure yields {} so credential management keeps working.
class OauthAppProviderDetails
  HTTP_TIMEOUT = 2
  MAX_BODY_BYTES = 128 * 1024

  attr_reader :app

  def initialize(app)
    @app = app
  end

  # The shared account's credential, or nil.
  def credential
    return @credential if defined?(@credential)

    @credential = (app.broker_credentials.order(created_at: :asc, id: :asc).first if app.shared?)
  end

  def details
    @details ||= fetch_details
  end

  private

  def fetch_details
    provider = app.provider_strategy
    return {} unless provider.respond_to?(:details_for) && fetchable?(credential)

    result = provider.details_for(credential, http_client: HttpClient.new(
      open_timeout: HTTP_TIMEOUT, read_timeout: HTTP_TIMEOUT, write_timeout: HTTP_TIMEOUT, max_body_bytes: MAX_BODY_BYTES
    ))
    result.is_a?(Hash) ? result : {}
  rescue StandardError
    # Optional provider details must never prevent credential management.
    {}
  end

  def fetchable?(credential)
    credential && !credential.dead? && credential.access_token.present? &&
      (credential.expires_at.nil? || credential.expires_at.future?)
  end
end
