require "base64"
require "digest"
require "uri"

module Mcp
  # MCP OAuth authorization server: metadata, dynamic client registration,
  # the authorize entry point, and the token endpoint. The consent page lives
  # in the SPA (/mcp/oauth/consent); GET /mcp/oauth/authorize validates the
  # request server-side, then hands the SPA a signed single-use consent ticket
  # (Mcp::ConsentTicket). The consent page posts the decision as a classic
  # form to POST /mcp/oauth/consent, which consumes the ticket, revalidates
  # the request, and answers with a 303 to the client's registered
  # redirect_uri: the authorization code never reaches page JavaScript and
  # the browser never script-navigates to a client URL.
  class OauthController < ApplicationController
    # Failure codes the SPA consent page maps to fixed messages.
    CONSENT_ERRORS = %w[csrf invalid_decision invalid_consent_request invalid_authorization_request server_error].freeze
    CONSENT_PATH = "/mcp/oauth/consent".freeze

    skip_before_action :require_login
    skip_before_action :require_active_account
    skip_forgery_protection only: %i[register token]
    # decide verifies the token itself so a failure lands on the consent page
    # (with a code) instead of raising.
    skip_forgery_protection only: :decide

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
      if current_user.disabled?
        reset_session
        return redirect_to login_path, alert: ConsoleSignIn::DISABLED_MESSAGE
      end

      authorization = AuthorizationRequest.from_params(params)
      return authorization_error(authorization) unless authorization.valid?

      ticket = ConsentTicket.issue(session, user: current_user, request: authorization)
      redirect_to "#{CONSENT_PATH}?#{{ request: ticket }.to_query}"
    end

    # POST /mcp/oauth/consent  (form: request, decision, authenticity_token)
    def decide
      return redirect_to(login_path, status: :see_other) unless current_user
      return redirect_to(pending_path, status: :see_other) if current_user.pending?
      if current_user.disabled?
        reset_session
        return redirect_to login_path, alert: ConsoleSignIn::DISABLED_MESSAGE, status: :see_other
      end
      return consent_failure("csrf") unless valid_request_origin? && any_authenticity_token_valid?

      decision = params[:decision].to_s
      # Checked before consuming, so a malformed post does not burn the ticket.
      return consent_failure("invalid_decision") unless %w[approve deny].include?(decision)

      authorization = ConsentTicket.consume(session, params[:request], user: current_user)
      return consent_failure("invalid_consent_request") unless authorization
      unless authorization.valid?
        url = authorization.error_redirect_url
        return url ? redirect_to(url, allow_other_host: true, status: :see_other) : consent_failure("invalid_authorization_request")
      end

      url = decision == "approve" ? authorization.issue_code!(current_user) : authorization.denied_redirect_url
      redirect_to url, allow_other_host: true, status: :see_other
    rescue ActiveRecord::RecordInvalid => e
      Rails.logger.error { "mcp authorization code issue failed: #{e.record.errors.full_messages.to_sentence}" }
      url = authorization&.error_redirect_url(:server_error, "The authorization code could not be issued.")
      url ? redirect_to(url, allow_other_host: true, status: :see_other) : consent_failure("server_error")
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
      remember_console_return_path(request.fullpath)
      redirect_to login_path
    end

    def consent_failure(code)
      redirect_to "#{CONSENT_PATH}?#{{ error: code }.to_query}", status: :see_other
    end

    # Invalid requests go back to the client when its redirect_uri is trusted;
    # otherwise the user sees the error (never an untrusted redirect).
    def authorization_error(authorization)
      url = authorization.error_redirect_url
      return redirect_to(url, allow_other_host: true) if url

      render plain: authorization.error_description, status: :bad_request
    end

    def oauth_error(error, description, status:)
      render json: { error: error.to_s, error_description: description }, status: status
    end

    def resolve_client(client_id)
      McpOauthClient.find_by_oid(client_id)
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
      AuthorizationRequest.normalize_scope_param(value, default)
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
