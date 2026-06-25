require "net/http"
require "json"
require "uri"

module Broker
  # RefreshClient performs raw RFC 6749 token grant POSTs against a token
  # endpoint and returns the parsed response. It owns no retry/backoff state --
  # BrokerCredential drives that. Ported from iron-token-broker's
  # internal/broker/refresh.go, with password-grant support added for providers
  # that still require it.
  #
  # SECURITY: this class never logs the refresh_token, client_secret, the
  # response body, or any decoded token. Callers must keep the same discipline.
  class RefreshClient
    # Normalized success result. expires_in is in seconds (nil if the IdP omitted
    # it -- the caller picks a conservative default).
    Result = Data.define(:access_token, :refresh_token, :expires_in)

    # The minimal HTTP response shape RefreshClient consumes, so tests can inject
    # a double without Net::HTTP.
    Response = Data.define(:status, :body)

    DEFAULT_TIMEOUT = 30
    MAX_BODY_BYTES = 64 * 1024

    # http: an optional callable for testing, invoked as
    #   http.call(url:, form:, headers:, timeout:, form_encoding:) -> Response
    # When nil, a Net::HTTP-backed implementation is used.
    def initialize(http: nil)
      @http = http
    end

    # Performs one token exchange. Raises Broker::RefreshError on any failure
    # (classified retryable vs. unrecoverable). scopes is an array; headers is a
    # name=>value hash applied verbatim to the token POST.
    def refresh(token_endpoint:, client_id: nil, grant: "refresh_token", refresh_token: nil,
                username: nil, password: nil, api_key: nil, client_secret: nil,
                scopes: [], headers: {}, timeout: DEFAULT_TIMEOUT)
      raise ArgumentError, "token endpoint is required" if token_endpoint.blank?
      raise ArgumentError, "client_id is required" if client_id.blank? && requires_client_id?(grant)

      form, form_encoding = form_for_grant(
        grant: grant,
        client_id: client_id,
        refresh_token: refresh_token,
        username: username,
        password: password,
        api_key: api_key
      )
      unless preqin_grant?(grant)
        form["client_secret"] = client_secret if client_secret.present?
        form["scope"] = scopes.join(" ") if scopes.present?
      end

      response = perform(token_endpoint, form, headers, timeout, form_encoding: form_encoding)

      if response.status / 100 != 2
        return classify_error(
          response.status,
          response.body,
          strict_4xx: preqin_grant?(grant)
        )
      end

      parse_success(response)
    end

    private

    def requires_client_id?(grant)
      !preqin_grant?(grant)
    end

    def preqin_grant?(grant)
      %w[preqin preqin_refresh_token].include?(grant)
    end

    def form_for_grant(grant:, client_id:, refresh_token:, username:, password:, api_key:)
      case grant
      when "refresh_token"
        raise ArgumentError, "refresh_token is required" if refresh_token.blank?

        [ {
          "grant_type" => "refresh_token",
          "refresh_token" => refresh_token,
          "client_id" => client_id
        }, :urlencoded ]
      when "password"
        raise ArgumentError, "username is required" if username.blank?
        raise ArgumentError, "password is required" if password.blank?

        [ {
          "grant_type" => "password",
          "username" => username,
          "password" => password,
          "client_id" => client_id
        }, :urlencoded ]
      when "preqin"
        raise ArgumentError, "username is required" if username.blank?
        raise ArgumentError, "api_key is required" if api_key.blank?

        [ {
          "username" => username,
          "apikey" => api_key
        }, :multipart ]
      when "preqin_refresh_token"
        raise ArgumentError, "refresh_token is required" if refresh_token.blank?

        [ { "refresh_token" => refresh_token }, :multipart ]
      else
        raise ArgumentError, "unsupported grant #{grant.inspect}"
      end
    end

    def perform(url, form, headers, timeout, form_encoding:)
      if @http
        return @http.call(url: url, form: form, headers: headers, timeout: timeout,
                          form_encoding: form_encoding)
      end

      uri = URI.parse(url)
      req = Net::HTTP::Post.new(uri)
      if form_encoding == :multipart
        req.set_form(form.to_a, "multipart/form-data")
      else
        req.set_form_data(form)
        req["Content-Type"] = "application/x-www-form-urlencoded"
      end
      req["Accept"] = "application/json"
      headers.each { |name, value| req[name] = value }

      http = Net::HTTP.new(uri.host, uri.port)
      http.use_ssl = uri.scheme == "https"
      http.open_timeout = timeout
      http.read_timeout = timeout

      res = http.request(req)
      Response.new(status: res.code.to_i, body: res.body.to_s.byteslice(0, MAX_BODY_BYTES))
    rescue StandardError => e
      # Network/transport failures are transient: a brief outage must not mark the
      # credential dead. Backoff exhaustion is the louder signal.
      raise RefreshError.new("token endpoint request failed: #{e.class}",
                             stage: "network", retryable: true)
    end

    def parse_success(response)
      parsed = JSON.parse(response.body)
      if parsed["ok"] == false && parsed["error"].present?
        raise RefreshError.new("token endpoint rejected credential: #{parsed["error"]}",
                               stage: "oauth", code: parsed["error"],
                               status: response.status, retryable: false)
      end

      access_token = parsed["access_token"]
      if access_token.blank?
        raise RefreshError.new("token endpoint returned an empty access_token",
                               stage: "parse", status: response.status, retryable: true)
      end
      expires_in = parsed["expires_in"]
      Result.new(
        access_token: access_token,
        refresh_token: parsed["refresh_token"], # nil/empty => IdP did not rotate
        expires_in: expires_in ? Integer(expires_in) : nil
      )
    rescue JSON::ParserError, ArgumentError, TypeError
      # A misbehaving gateway can corrupt a 2xx body without the credential being
      # invalid. Treat as transient; the dead-after-backoff escalation still
      # catches a persistently broken IdP.
      raise RefreshError.new("parsing token response failed",
                             stage: "parse", status: response.status, retryable: true)
    end

    # Ported from classifyTokenEndpointError. Aggressive on the non-retryable
    # side: any RFC 6749 5.2 error code is structural and means the credential is
    # dead until a human acts. Transport-shaped failures (5xx, bodyless 4xx) are
    # retryable.
    def classify_error(status, body, strict_4xx: false)
      oauth_error = begin
        JSON.parse(body.to_s)["error"]
      rescue JSON::ParserError, TypeError
        nil
      end

      if status / 100 == 5 || status == 429
        raise RefreshError.new("token endpoint http #{status}",
                               stage: "http", code: oauth_error.presence, status: status, retryable: true)
      end

      if oauth_error.blank?
        if strict_4xx
          raise RefreshError.new("token endpoint http #{status}",
                                 stage: "http", code: "http_#{status}",
                                 status: status, retryable: false)
        end
        # 4xx with no OAuth body: most likely a gateway/rate-limiter, not the IdP.
        raise RefreshError.new("token endpoint http #{status}",
                               stage: "http", status: status, retryable: true)
      end

      raise RefreshError.new("token endpoint rejected credential: #{oauth_error}",
                             stage: "oauth", code: oauth_error, status: status, retryable: false)
    end
  end
end
