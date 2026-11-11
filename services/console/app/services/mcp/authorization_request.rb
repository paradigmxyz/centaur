require "uri"

module Mcp
  # A validated MCP OAuth authorization request (RFC 6749 authorization code
  # grant with mandatory PKCE S256 and an RFC 8707 resource indicator).
  #
  # Built from the raw query parameters of GET /mcp/oauth/authorize and rebuilt
  # from the same parameters (carried in a signed consent token) when the user
  # approves or denies, so every check runs again server-side at decision time:
  # client exists, response_type=code, redirect_uri registered for the client,
  # S256 challenge present, scopes supported, resource matches the configured
  # MCP resource. The browser never chooses a redirect target: redirects only go
  # to a redirect_uri the client registered.
  class AuthorizationRequest
    PARAM_KEYS = %w[
      response_type client_id redirect_uri scope state resource code_challenge code_challenge_method
    ].freeze

    attr_reader :params, :client, :scopes, :resource, :error, :error_description

    def self.from_params(raw)
      hash = raw.respond_to?(:to_unsafe_h) ? raw.to_unsafe_h : raw.to_h
      new(hash.stringify_keys.slice(*PARAM_KEYS).transform_values(&:to_s))
    end

    def initialize(params)
      @params = params
      validate
    end

    def valid?
      error.nil?
    end

    def redirect_uri = params["redirect_uri"].to_s
    def state = params["state"].presence
    def code_challenge = params["code_challenge"].to_s

    # The client may receive an error redirect only once both it and the
    # redirect_uri check out; otherwise the error is shown to the user.
    def error_redirectable?
      client.present? && client.redirect_uri_allowed?(redirect_uri)
    end

    # The client's redirect_uri carrying +error+ (defaults to the validation
    # error), or nil when the redirect_uri is not trusted for this client.
    def error_redirect_url(error = self.error, description = error_description)
      return nil unless error_redirectable?

      redirect_with("error" => error.to_s, "error_description" => description)
    end

    def denied_redirect_url
      error_redirect_url(:access_denied, "The user denied the authorization request.")
    end

    def redirect_host
      URI.parse(redirect_uri).host.presence || redirect_uri
    rescue URI::InvalidURIError
      redirect_uri
    end

    # The parameters as validated (scope and resource normalized), suitable for
    # carrying to the consent decision.
    def normalized_params
      params.merge("scope" => scopes.join(" "), "resource" => resource.to_s)
    end

    # Mints a single-use authorization code for +user+ and returns the client
    # redirect URL carrying it. The principal is the user's console principal
    # (provisioned and seeded with the user-mcp role on first use).
    def issue_code!(user)
      raise ArgumentError, "invalid authorization request" unless valid?

      code = McpOauthAuthorizationCode.create!(
        mcp_oauth_client: client,
        user: user,
        principal: ConsoleUserPrincipalProvisioner.call(user),
        redirect_uri: redirect_uri,
        code_challenge: code_challenge,
        resource: resource,
        scopes: scopes
      )
      client.touch(:last_used_at)
      redirect_with("code" => code.plaintext_code)
    end

    def self.normalize_scope_param(value, default)
      return default if value.blank?
      value.to_s.split(/[,\s]+/).map(&:strip).reject(&:blank?)
    end

    def self.configured_resource_url
      normalize_resource_url(ENV["CENTAUR_MCP_PUBLIC_URL"].presence || ConsoleEnv["MCP_PUBLIC_URL"].presence)
    end

    def self.normalize_resource_url(value)
      uri = URI.parse(value.to_s.strip)
      return nil unless %w[http https].include?(uri.scheme) && uri.host.present?
      uri.fragment = nil
      path = uri.path.to_s.sub(%r{/+\z}, "")
      uri.path = path.end_with?("/mcp") ? path : "#{path}/mcp"
      uri.to_s.sub(/\?\z/, "")
    rescue URI::InvalidURIError
      nil
    end

    private

    def validate
      @client = McpOauthClient.find_by_oid(params["client_id"])
      return fail_with(:invalid_request, "Unknown client.") unless client
      unless params["response_type"] == "code"
        return fail_with(:unsupported_response_type, "Only response_type=code is supported.")
      end
      unless client.redirect_uri_allowed?(redirect_uri)
        return fail_with(:invalid_request, "redirect_uri is not registered for this client.")
      end
      unless params["code_challenge_method"] == "S256"
        return fail_with(:invalid_request, "code_challenge_method must be S256.")
      end
      return fail_with(:invalid_request, "code_challenge is required.") if code_challenge.blank?

      @scopes = self.class.normalize_scope_param(params["scope"], McpOauthClient::DEFAULT_SCOPES)
      unsupported = scopes - McpOauthClient::DEFAULT_SCOPES
      return fail_with(:invalid_scope, "Unsupported scope: #{unsupported.join(' ')}.") if unsupported.any?

      @resource = resolve_resource
      fail_with(:invalid_target, "resource is required.") if resource.blank?
    end

    def fail_with(error, description)
      @error = error
      @error_description = description
      nil
    end

    # Fail closed: without a configured canonical resource URL we would
    # otherwise mint tokens bound to any caller-supplied audience.
    def resolve_resource
      configured = self.class.configured_resource_url
      return nil if configured.blank?
      requested = params["resource"].presence
      return nil if requested.present? && self.class.normalize_resource_url(requested) != configured
      configured
    end

    def redirect_with(extra)
      uri = URI.parse(redirect_uri)
      query = Rack::Utils.parse_nested_query(uri.query)
      query.merge!(extra)
      query["state"] = state if state
      uri.query = query.to_query
      uri.to_s
    end
  end
end
