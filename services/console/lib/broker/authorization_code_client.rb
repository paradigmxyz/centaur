require "net/http"
require "json"
require "uri"

module Broker
  # Performs the RFC 6749 4.1.3 authorization_code grant POST (optionally with PKCE) and
  # returns the parsed response. Used once per consent flow; it owns no
  # retry/backoff state -- a consent flow is synchronous and any failure surfaces
  # to the end user as a redirect. Provider-agnostic: the caller supplies the
  # token endpoint from the provider strategy.
  #
  # SECURITY: this class never logs the code, tokens, client_secret, or the
  # response body. Callers must keep the same discipline. Mirrors RefreshClient's
  # injectable-HTTP design and 64 KiB body cap.
  class AuthorizationCodeClient
    # Normalized success result. scope is the granted-scope string in the
    # provider's native format (nil if omitted); id_token is the raw JWT (nil if
    # absent -- the provider strategy decides whether that is fatal). response is
    # the parsed provider response for provider-specific metadata.
    Result = Data.define(:access_token, :refresh_token, :expires_in, :scope, :id_token, :response)

    # The minimal HTTP response shape consumed, so tests can inject a double
    # without Net::HTTP.
    Response = Data.define(:status, :body)

    DEFAULT_TIMEOUT = 30
    MAX_BODY_BYTES = 64 * 1024

    # http: an optional callable for testing, invoked as
    #   http.call(url:, form:, headers:, timeout:) -> Response
    # When nil, a Net::HTTP-backed implementation is used.
    def initialize(http: nil)
      @http = http
    end

    # Exchanges an authorization code for tokens. Raises Broker::ExchangeError on
    # any failure (non-2xx, unparseable body, empty access_token, or -- when
    # require_refresh_token is true -- a missing refresh_token).
    #
    # require_refresh_token defaults to true for the broker consent flow, where
    # access_type=offline + prompt=consent always return one and its absence means
    # the app is misconfigured. The console-login flow passes false: it requests no
    # offline access and only needs the id_token to identify the operator.
    def exchange(token_endpoint:, client_id:, client_secret:, code:, redirect_uri:,
                 code_verifier:, timeout: DEFAULT_TIMEOUT, require_refresh_token: true)
      raise ArgumentError, "token endpoint is required" if token_endpoint.blank?
      raise ArgumentError, "client_id is required" if client_id.blank?
      raise ArgumentError, "code is required" if code.blank?
      raise ArgumentError, "redirect_uri is required" if redirect_uri.blank?
      form = {
        "grant_type" => "authorization_code",
        "code" => code,
        "client_id" => client_id,
        "redirect_uri" => redirect_uri
      }
      form["client_secret"] = client_secret if client_secret.present?
      form["code_verifier"] = code_verifier if code_verifier.present?

      response = perform(token_endpoint, form, timeout)

      classify_error(response.status, response.body) if response.status / 100 != 2

      parse_success(response, require_refresh_token: require_refresh_token)
    end

    private

    def perform(url, form, timeout)
      if @http
        return @http.call(url: url, form: form, headers: {}, timeout: timeout)
      end

      uri = URI.parse(url)
      req = Net::HTTP::Post.new(uri)
      req.set_form_data(form)
      req["Content-Type"] = "application/x-www-form-urlencoded"
      req["Accept"] = "application/json"

      http = Net::HTTP.new(uri.host, uri.port)
      http.use_ssl = uri.scheme == "https"
      http.open_timeout = timeout
      http.read_timeout = timeout

      res = http.request(req)
      Response.new(status: res.code.to_i, body: res.body.to_s.byteslice(0, MAX_BODY_BYTES))
    rescue StandardError => e
      raise ExchangeError.new("token endpoint request failed: #{e.class}", stage: "network")
    end

    def parse_success(response, require_refresh_token:)
      parsed = JSON.parse(response.body)
      if parsed["ok"] == false && parsed["error"].present?
        raise ExchangeError.new("token endpoint rejected code: #{parsed["error"]}",
                                stage: "oauth", code: parsed["error"], status: response.status)
      end

      token_payload = parsed["authed_user"].is_a?(Hash) && parsed.dig("authed_user", "access_token").present? ? parsed["authed_user"] : parsed
      access_token = token_payload["access_token"]
      if access_token.blank?
        raise ExchangeError.new("token endpoint returned an empty access_token",
                                stage: "parse", status: response.status)
      end

      refresh_token = token_payload["refresh_token"]
      if require_refresh_token && refresh_token.blank?
        # With access_type=offline + prompt=consent a refresh token is always
        # returned; its absence means the app is misconfigured at the IdP. The
        # login flow opts out of this check (it requests no offline access).
        raise ExchangeError.new("token endpoint returned no refresh_token",
                                stage: "oauth", code: "missing_refresh_token", status: response.status)
      end

      expires_in = token_payload["expires_in"]
      Result.new(
        access_token: access_token,
        refresh_token: refresh_token,
        expires_in: expires_in ? Integer(expires_in) : nil,
        scope: token_payload["scope"] || parsed["scope"],
        id_token: token_payload["id_token"] || parsed["id_token"],
        response: parsed
      )
    rescue JSON::ParserError, ArgumentError, TypeError
      raise ExchangeError.new("parsing token response failed", stage: "parse", status: response.status)
    end

    def classify_error(status, body)
      oauth_error = begin
        JSON.parse(body.to_s)["error"]
      rescue JSON::ParserError, TypeError
        nil
      end

      raise ExchangeError.new("token endpoint http #{status}",
                              stage: oauth_error.present? ? "oauth" : "http",
                              code: oauth_error.presence, status: status)
    end
  end
end
