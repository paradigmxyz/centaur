require "base64"
require "digest"
require "uri"

module Mcp
  class OauthController < ApplicationController
    layout "auth"

    skip_before_action :require_login, only: %i[metadata register authorize token]
    skip_before_action :require_active_account, only: %i[metadata register authorize token]
    skip_forgery_protection only: %i[register token]

    ACCESS_TOKEN_TTL_SECONDS = 1.hour.to_i

    # GET /.well-known/oauth-authorization-server
    def metadata
      render json: {
        issuer: public_base_url,
        authorization_endpoint: URI.join(public_base_url, "/mcp/oauth/authorize").to_s,
        token_endpoint: URI.join(public_base_url, "/mcp/oauth/token").to_s,
        registration_endpoint: URI.join(public_base_url, "/mcp/oauth/register").to_s,
        response_types_supported: [ "code" ],
        grant_types_supported: McpOauthClient::DEFAULT_GRANT_TYPES,
        code_challenge_methods_supported: [ "S256" ],
        token_endpoint_auth_methods_supported: [ "none" ],
        scopes_supported: McpOauthClient::DEFAULT_SCOPES,
        resource_parameter_supported: true
      }
    end

    # POST /mcp/oauth/register
    def register
      requested_redirect_uris =
        Array(params[:redirect_uris]).map(&:to_s).map(&:strip).reject(&:blank?)
      client = McpOauthClient.create!(
        name: params[:client_name].presence || "MCP client",
        redirect_uris: requested_redirect_uris,
        grant_types: normalize_list_param(
          params[:grant_types],
          McpOauthClient::DEFAULT_GRANT_TYPES
        ),
        response_types: normalize_list_param(
          params[:response_types],
          McpOauthClient::DEFAULT_RESPONSE_TYPES
        ),
        scopes: normalize_scope_param(params[:scope], McpOauthClient::DEFAULT_SCOPES),
        metadata: registration_metadata
      )

      render json: {
        client_id: client.public_client_id,
        client_name: client.name,
        redirect_uris: client.redirect_uris,
        grant_types: client.grant_types,
        response_types: client.response_types,
        scope: client.scopes.join(" "),
        token_endpoint_auth_method: "none"
      }, status: :created
    rescue ActiveRecord::RecordInvalid => e
      oauth_error(
        :invalid_client_metadata,
        e.record.errors.full_messages.to_sentence,
        status: :bad_request
      )
    end

    # GET /mcp/oauth/authorize
    def authorize
      return redirect_to_login unless current_user
      return redirect_to pending_path if current_user.pending?
      return redirect_to login_path, alert: "Your account is disabled." if current_user.disabled?

      authorization = validated_authorization_request
      return unless authorization

      assign_authorization_view(authorization)
      render :authorize
    end

    # POST /mcp/oauth/authorize
    def approve
      authorization = validated_authorization_request
      return unless authorization

      unless params[:decision] == "approve"
        return authorization_error(
          authorization[:client],
          :access_denied,
          "The user denied the authorization request."
        )
      end

      issue_authorization_code(authorization)
    rescue ActiveRecord::RecordInvalid => e
      authorization_error(nil, :server_error, e.record.errors.full_messages.to_sentence)
    end

    # POST /mcp/oauth/token
    def token
      case params[:grant_type]
      when "authorization_code"
        exchange_authorization_code
      when "refresh_token"
        exchange_refresh_token
      else
        oauth_error(:unsupported_grant_type, "Unsupported grant_type.", status: :bad_request)
      end
    end

    private

    def validated_authorization_request
      client = resolve_client(params[:client_id])
      return authorization_request_error(nil, :invalid_request, "Unknown client.") unless client

      unless params[:response_type] == "code"
        return authorization_request_error(
          client,
          :unsupported_response_type,
          "Only response_type=code is supported."
        )
      end
      unless client.redirect_uri_allowed?(params[:redirect_uri])
        return authorization_request_error(
          client,
          :invalid_request,
          "redirect_uri is not registered for this client."
        )
      end
      unless params[:code_challenge_method] == "S256"
        return authorization_request_error(
          client,
          :invalid_request,
          "code_challenge_method must be S256."
        )
      end
      if params[:code_challenge].blank?
        return authorization_request_error(client, :invalid_request, "code_challenge is required.")
      end

      scopes = normalize_scope_param(params[:scope], McpOauthClient::DEFAULT_SCOPES)
      unsupported = scopes - McpOauthClient::DEFAULT_SCOPES
      if unsupported.any?
        return authorization_request_error(
          client,
          :invalid_scope,
          "Unsupported scope: #{unsupported.join(' ')}."
        )
      end

      resource = resolve_requested_resource
      return authorization_request_error(client, :invalid_target, "resource is required.") if resource.blank?

      { client: client, scopes: scopes, resource: resource }
    end

    def authorization_request_error(client, error, description)
      authorization_error(client, error, description)
      nil
    end

    def assign_authorization_view(authorization)
      @client = authorization[:client]
      @scopes = authorization[:scopes]
      @resource = authorization[:resource]
      @redirect_uri = params[:redirect_uri].to_s
      @redirect_host = redirect_uri_host(@redirect_uri)
      @authorization_params = authorization_form_params(authorization)
    end

    def issue_authorization_code(authorization)
      client = authorization[:client]
      principal = principal_for_current_user
      code = McpOauthAuthorizationCode.create!(
        mcp_oauth_client: client,
        user: current_user,
        principal: principal,
        redirect_uri: params[:redirect_uri].to_s,
        code_challenge: params[:code_challenge].to_s,
        resource: authorization[:resource],
        scopes: authorization[:scopes]
      )
      client.touch(:last_used_at)

      uri = URI.parse(params[:redirect_uri])
      query = Rack::Utils.parse_nested_query(uri.query)
      query["code"] = code.plaintext_code
      query["state"] = params[:state] if params[:state].present?
      uri.query = query.to_query
      redirect_to uri.to_s, allow_other_host: true
    end

    def authorization_form_params(authorization)
      {
        response_type: params[:response_type].to_s,
        client_id: authorization[:client].public_client_id,
        redirect_uri: params[:redirect_uri].to_s,
        scope: authorization[:scopes].join(" "),
        state: params[:state].to_s,
        resource: authorization[:resource],
        code_challenge: params[:code_challenge].to_s,
        code_challenge_method: params[:code_challenge_method].to_s
      }
    end

    def redirect_uri_host(value)
      URI.parse(value).host
    rescue URI::InvalidURIError
      value
    end

    def exchange_authorization_code
      client = resolve_client(params[:client_id])
      return oauth_error(:invalid_client, "Unknown client.", status: :unauthorized) unless client
      code = McpOauthAuthorizationCode.find_usable(params[:code])
      unless code
        return oauth_error(
          :invalid_grant,
          "Authorization code is invalid or expired.",
          status: :bad_request
        )
      end
      unless code.mcp_oauth_client == client
        return oauth_error(
          :invalid_grant,
          "Authorization code was not issued to this client.",
          status: :bad_request
        )
      end
      unless code.redirect_uri == params[:redirect_uri].to_s
        return oauth_error(
          :invalid_grant,
          "redirect_uri does not match the authorization request.",
          status: :bad_request
        )
      end
      unless pkce_valid?(code.code_challenge, params[:code_verifier].to_s)
        return oauth_error(:invalid_grant, "PKCE verification failed.", status: :bad_request)
      end

      refresh = nil
      invalid_grant = false
      inactive_user = false
      McpOauthAuthorizationCode.transaction do
        code.lock!
        if code.consumed_at.present? || code.expires_at <= Time.current
          invalid_grant = true
        elsif !code.user.active?
          inactive_user = true
          code.consume!
          code.user.revoke_mcp_oauth_refresh_tokens!
        else
          code.consume!
          refresh = McpOauthRefreshToken.create!(
            mcp_oauth_client: client,
            user: code.user,
            principal: code.principal,
            resource: code.resource,
            scopes: code.scopes
          )
        end
      end
      if invalid_grant
        return oauth_error(
          :invalid_grant,
          "Authorization code is invalid or expired.",
          status: :bad_request
        )
      end
      if inactive_user
        return oauth_error(
          :invalid_grant,
          "User account is not active.",
          status: :bad_request
        )
      end

      client.touch(:last_used_at)
      render_token_response(
        client: client,
        user: code.user,
        principal: code.principal,
        resource: code.resource,
        scopes: code.scopes,
        refresh_token: refresh.plaintext_token
      )
    end

    def exchange_refresh_token
      client = resolve_client(params[:client_id])
      return oauth_error(:invalid_client, "Unknown client.", status: :unauthorized) unless client
      refresh = McpOauthRefreshToken.find_usable(params[:refresh_token])
      unless refresh
        return oauth_error(
          :invalid_grant,
          "Refresh token is invalid or expired.",
          status: :bad_request
        )
      end
      unless refresh.mcp_oauth_client == client
        return oauth_error(
          :invalid_grant,
          "Refresh token was not issued to this client.",
          status: :bad_request
        )
      end

      rotated = nil
      invalid_grant = false
      inactive_user = false
      McpOauthRefreshToken.transaction do
        refresh.lock!
        if refresh.revoked_at.present? || refresh.expires_at <= Time.current
          invalid_grant = true
        elsif !refresh.user.active?
          inactive_user = true
          refresh.user.revoke_mcp_oauth_refresh_tokens!
        else
          refresh.update!(revoked_at: Time.current, last_used_at: Time.current)
          rotated = McpOauthRefreshToken.create!(
            mcp_oauth_client: client,
            user: refresh.user,
            principal: refresh.principal,
            resource: refresh.resource,
            scopes: refresh.scopes
          )
        end
      end
      if invalid_grant
        return oauth_error(
          :invalid_grant,
          "Refresh token is invalid or expired.",
          status: :bad_request
        )
      end
      if inactive_user
        return oauth_error(
          :invalid_grant,
          "User account is not active.",
          status: :bad_request
        )
      end

      client.touch(:last_used_at)
      render_token_response(
        client: client,
        user: refresh.user,
        principal: refresh.principal,
        resource: refresh.resource,
        scopes: refresh.scopes,
        refresh_token: rotated.plaintext_token
      )
    end

    def render_token_response(client:, user:, principal:, resource:, scopes:, refresh_token:)
      now = Time.current.to_i
      ttl = access_token_ttl_seconds
      payload = {
        iss: public_base_url,
        sub: user.oid,
        aud: resource,
        exp: now + ttl,
        nbf: now - 5,
        iat: now,
        jti: "mcpjwt_#{SecureRandom.hex(16)}",
        scope: scopes.join(" "),
        client_id: client.public_client_id,
        principal_id: principal.oid,
        principal_foreign_id: principal.foreign_id,
        email: user.email,
        name: user.name.presence || user.email
      }
      render json: {
        access_token: Mcp::Jwt.encode(payload),
        token_type: "Bearer",
        expires_in: ttl,
        scope: scopes.join(" "),
        refresh_token: refresh_token
      }
    rescue KeyError => e
      oauth_error(:server_error, e.message, status: :service_unavailable)
    end

    def redirect_to_login
      session[:return_to] = request.fullpath if request.request_method == "GET"
      redirect_to login_path
    end

    def authorization_error(client, error, description)
      if client&.redirect_uri_allowed?(params[:redirect_uri])
        uri = URI.parse(params[:redirect_uri])
        query = Rack::Utils.parse_nested_query(uri.query)
        query["error"] = error.to_s
        query["error_description"] = description
        query["state"] = params[:state] if params[:state].present?
        uri.query = query.to_query
        redirect_to uri.to_s, allow_other_host: true
      else
        render plain: description, status: :bad_request
      end
    end

    def oauth_error(error, description, status:)
      render json: { error: error.to_s, error_description: description }, status: status
    end

    def resolve_client(client_id)
      McpOauthClient.find_by_oid(client_id)
    end

    def resolve_requested_resource
      # Fail closed: without a configured canonical resource URL we would
      # otherwise mint tokens bound to any caller-supplied audience.
      configured = normalize_mcp_resource_url(configured_mcp_resource_url)
      return nil if configured.blank?
      requested = params[:resource].presence
      return nil if requested.present? && normalize_mcp_resource_url(requested) != configured
      configured
    end

    def configured_mcp_resource_url
      ENV["CENTAUR_MCP_PUBLIC_URL"].presence || ConsoleEnv["MCP_PUBLIC_URL"].presence
    end

    def normalize_mcp_resource_url(value)
      uri = URI.parse(value.to_s.strip)
      return nil unless %w[http https].include?(uri.scheme) && uri.host.present?
      uri.fragment = nil
      path = uri.path.to_s.sub(%r{/+\z}, "")
      uri.path = path.end_with?("/mcp") ? path : "#{path}/mcp"
      uri.to_s.sub(/\?\z/, "")
    rescue URI::InvalidURIError
      nil
    end

    def principal_for_current_user
      foreign_id = principal_foreign_id(current_user.email)
      Principal
        .find_or_initialize_by(namespace: mcp_principal_namespace, foreign_id: foreign_id)
        .tap do |principal|
        principal.created_by ||= current_user
        principal.name = current_user.name.presence || current_user.email
        principal.labels = principal.labels.merge(
          "managed-by" => "centaur",
          "kind" => "console_user",
          "console-user-id" => current_user.oid,
          "email" => current_user.email
        )
        principal.save!
      end
    end

    def principal_foreign_id(email)
      normalized = email.to_s.downcase.strip
      safe = normalized.gsub(/[^A-Za-z0-9\-._~]/, "-").gsub(/-+/, "-").first(48)
      digest = Digest::SHA256.hexdigest(normalized).first(12)
      "console-user-#{safe}-#{digest}"
    end

    def mcp_principal_namespace
      ENV["CENTAUR_MCP_PRINCIPAL_NAMESPACE"].presence ||
        ConsoleEnv["MCP_PRINCIPAL_NAMESPACE"].presence ||
        "default"
    end

    def access_token_ttl_seconds
      raw =
        ENV["CENTAUR_MCP_ACCESS_TOKEN_TTL_SECONDS"].presence ||
        ConsoleEnv["MCP_ACCESS_TOKEN_TTL_SECONDS"].presence
      seconds = raw.to_i
      seconds.positive? ? seconds : ACCESS_TOKEN_TTL_SECONDS
    end

    def registration_metadata
      params
        .to_unsafe_h
        .slice("client_uri", "logo_uri", "contacts", "software_id", "software_version")
        .compact
    end

    def normalize_list_param(value, default)
      list = value.presence || default
      Array(list).map(&:to_s).map(&:strip).reject(&:blank?).presence || default
    end

    def normalize_scope_param(value, default)
      return default if value.blank?
      value.to_s.split(/[,\s]+/).map(&:strip).reject(&:blank?)
    end

    def pkce_valid?(challenge, verifier)
      return false if verifier.blank?
      actual = Base64.urlsafe_encode64(Digest::SHA256.digest(verifier), padding: false)
      ActiveSupport::SecurityUtils.secure_compare(actual, challenge.to_s)
    rescue ArgumentError
      false
    end
  end
end
