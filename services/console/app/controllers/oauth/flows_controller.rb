require "base64"
require "digest"
require "uri"

module Oauth
  # The OAuth consent flow, keyed by an app's well-known slug:
  # /oauth/:slug/start sends a team member to the IdP's consent screen, and
  # /oauth/:slug/callback turns the returned authorization code into a managed
  # BrokerCredential linked to the OauthApp, then sends the user back to the
  # console Integrations page (or renders a result page on failure).
  #
  # Deliberately unauthenticated -- a team member connects an integration by
  # clicking a well-known link; there is no external app to integrate with, so
  # there is no return_to or user key. Safety comes from: a credential is only
  # minted after a successful consent + code exchange, and re-consent for the
  # same (app, provider account) upserts the existing credential. All
  # provider-specific behavior comes from the strategy (Oauth::Providers), which
  # is derived from the app.
  #
  # SECURITY: never logs the code, tokens, client_secret, or response bodies --
  # only oids and error codes, like the rest of the Broker/Oauth subsystem.
  class FlowsController < ApplicationController
    layout "auth"

    skip_before_action :require_login
    skip_before_action :require_active_account

    # The message_verifier purpose binding the signed state to this flow, the
    # state/cookie lifetime, and the encrypted cookie that ties a callback back to
    # the browser that started it.
    STATE_PURPOSE = :oauth_consent_flow
    FLOW_TTL = 10.minutes
    FLOW_COOKIE = :oauth_flow

    # Tests swap in an AuthorizationCodeClient built around an http double,
    # mirroring BrokerCredential#refresh_client.
    class_attribute :exchange_client_factory, default: -> { Broker::AuthorizationCodeClient.new }

    before_action :set_app

    # GET /oauth/:slug/start?scopes=
    def start
      return render_result(:error, status: :unprocessable_entity, message: "This integration is disabled.") unless @app.enabled?

      requested_scopes = parse_scopes(params[:scopes]) || Array(@app.allowed_scopes)
      unless @app.scopes_allowed?(requested_scopes)
        return render_result(:error, status: :unprocessable_entity, message: "One or more requested scopes are not allowed for this integration.")
      end

      nonce = SecureRandom.urlsafe_base64(32)
      code_verifier = SecureRandom.urlsafe_base64(64)

      state = Rails.application.message_verifier(STATE_PURPOSE).generate(
        { "app" => @app.oid, "scopes" => requested_scopes, "nonce" => nonce },
        purpose: STATE_PURPOSE, expires_in: FLOW_TTL
      )

      # :lax is required -- the callback arrives via a top-level cross-site
      # redirect from the IdP, which Lax permits for GET.
      cookies.encrypted[FLOW_COOKIE] = {
        value: { "nonce" => nonce, "code_verifier" => code_verifier }.to_json,
        expires: FLOW_TTL.from_now, httponly: true, same_site: :lax
      }

      redirect_to authorization_url(requested_scopes, state, code_verifier), allow_other_host: true
    end

    # GET /oauth/:slug/callback?code=&state=  (or ?error=)
    def callback
      state = Rails.application.message_verifier(STATE_PURPOSE).verified(params[:state], purpose: STATE_PURPOSE)
      return render_result(:error, status: :bad_request, message: "This consent link is invalid or has expired. Start again.") if state.nil?

      # The signed state must belong to this slug's app and the app must still be
      # active.
      if state["app"] != @app.oid || !@app.enabled?
        return render_result(:error, status: :bad_request, message: "This integration is no longer available.")
      end

      flow = read_and_clear_flow_cookie
      if flow.nil? || flow["nonce"] != state["nonce"]
        return render_result(:error, status: :bad_request, message: "This flow expired or was started in another browser. Start again.")
      end

      # The user declined (or another IdP-side error).
      if params[:error].present?
        return render_result(:denied, message: "Consent was not granted (#{params[:error]}).")
      end

      result = exchange_code(params[:code], flow["code_verifier"])
      identity = @provider.identity_from(result, client_id: @app.client_id)
      @credential = upsert_credential(state, result, identity)
      enqueue_identity_enrichment(@credential)

      # Back to the Integrations page the user started from; failures below
      # still render the standalone result page, which offers a retry link.
      connected_as = " as #{identity[:email]}" if identity[:email].present?
      redirect_to console_integrations_path, notice: "#{@app.slug} connected#{connected_as}."
    rescue Broker::ExchangeError => e
      render_result(:error, message: "Connecting the integration failed (#{e.reason}).")
    rescue ActiveRecord::RecordInvalid => e
      # Most likely the deterministic foreign_id collides with an unrelated
      # credential. Log the messages only -- never token values.
      Rails.logger.error { "oauth flow credential save failed: #{e.record.errors.full_messages.to_sentence}" }
      render_result(:error, message: "Connecting the integration failed while saving the credential.")
    rescue ActiveRecord::RecordNotUnique
      # A concurrent consent for the same account won the unique index on the
      # credential or its wrapping secret. The winner's record is already saved, so
      # the user just needs to retry; the next consent upserts cleanly.
      render_result(:error, message: "Another consent for this account is in progress. Try again.")
    end

    private

    # Resolves the app from the well-known slug and derives its provider strategy.
    def set_app
      @app = OauthApp.find_by(slug: params[:slug])
      return render_result(:error, status: :not_found, message: "Unknown integration.") if @app.nil?
      @provider = @app.provider_strategy
      render_result(:error, status: :not_found, message: "Unknown integration.") if @provider.nil?
    end

    # Accepts space- or comma-separated scope lists; nil when none were given
    # (the caller defaults to the app's full allowlist).
    def parse_scopes(raw)
      return nil if raw.blank?
      raw.split(/[,\s]+/).map(&:strip).reject(&:blank?)
    end

    def authorization_url(requested_scopes, state, code_verifier)
      challenge = Base64.urlsafe_encode64(Digest::SHA256.digest(code_verifier), padding: false)
      query = {
        "client_id" => @app.client_id,
        "redirect_uri" => oauth_callback_redirect_uri(@app.slug),
        "response_type" => "code",
        "state" => state,
        "code_challenge" => challenge,
        "code_challenge_method" => "S256"
      }.merge(@provider.extra_authorization_params)
      query[@provider.authorization_scope_param] = (requested_scopes | @provider.identity_scopes).join(@provider.scope_separator)

      uri = URI.parse(@provider.authorization_endpoint)
      uri.query = URI.encode_www_form(query)
      uri.to_s
    end

    def exchange_code(code, code_verifier)
      exchange_client_factory.call.exchange(
        token_endpoint: @provider.token_endpoint,
        client_id: @app.client_id,
        client_secret: @app.client_secret,
        code: code.to_s,
        redirect_uri: oauth_callback_redirect_uri(@app.slug),
        code_verifier: code_verifier.to_s,
        require_refresh_token: @provider.refreshable?
      )
    end

    # Upserts one credential per (app, provider account). A new record gets its
    # identity/endpoint fixed (and an auto-generated external_user_key, since the
    # flow has no caller-supplied user); every consent (re)applies the rotating
    # blob, including the freshly-exchanged access token so the credential is live
    # immediately, and revives a dead credential.
    def upsert_credential(state, result, identity)
      BrokerCredential.transaction do
        credential = BrokerCredential.find_or_initialize_by(oauth_app: @app, provider_subject: identity[:subject])
        # When the consenting browser carries a signed-in console session,
        # remember which user connected this account. The Integrations page
        # matches on it, so the card flips to "Connected" even when the
        # provider account's email differs from the console login email.
        # Never overwritten: the first linked user keeps the credential.
        credential.created_by ||= current_user
        if credential.new_record?
          credential.namespace = @app.credential_namespace
          credential.foreign_id = "#{@app.provider}-#{@app.slug}-#{identity[:subject].downcase}"
          credential.name = "#{@provider.display_name} – #{identity_display_name(identity)}"
          credential.token_endpoint = @provider.token_endpoint
          credential.external_user_key = SecureRandom.urlsafe_base64(16)
        end

        now = Time.current
        expires_in = result.expires_in&.positive? ? result.expires_in : BrokerCredential::DEFAULT_EXPIRES_IN_SECONDS
        credential.assign_attributes(
          provider_email: identity[:email],
          # Store exactly what the IdP granted, so the refresh POST re-requests it.
          scopes: granted_scopes(result, state),
          refresh_token: result.refresh_token,
          access_token: result.access_token,
          expires_at: now + expires_in,
          last_refresh: now,
          failure_count: 0, dead: false, dead_reason: nil
        )
        credential.next_attempt_at = @provider.refreshable? ? credential.compute_next_attempt_at(now: now) : nil
        credential.save!
        ensure_wrapping_secret(credential)
        credential
      end
    end

    def granted_scopes(result, state)
      return Array(state["scopes"]) if result.scope.blank?
      @provider.parse_granted_scopes(result.scope)
    end

    def identity_display_name(identity)
      identity[:name].presence || identity[:email].presence || identity[:subject]
    end

    def enqueue_identity_enrichment(credential)
      case @app.provider
      when Oauth::Providers::Slack::KEY
        Oauth::EnrichCredentialIdentityJob.perform_later(credential.id)
      when Oauth::Providers::Github::KEY
        Oauth::EnrichGithubCredentialIdentityJob.perform_later(credential.id)
      end
    end

    # Wraps a minted credential in a grantable static secret, so an operator can
    # grant the integration's token to a principal straight from the console (a
    # broker credential is not grantable on its own). The secret injects the live
    # access token as `Authorization: Bearer <token>` through a token_broker source
    # pointing at the credential, scoped to the provider's API hosts. The token
    # stays fresh because the source resolves the credential live at sync time.
    #
    # Created once per credential (keyed on the broker_credential association, which
    # a unique index enforces) and left untouched on re-consent, so any operator
    # edits -- a different header, extra rules -- survive. Has no created_by:
    # unlike the credential, no console feature keys off the secret's owner. Left
    # without a foreign_id: it is found by association, and copying the credential's
    # would risk colliding with an operator-created secret.
    def ensure_wrapping_secret(credential)
      secret = StaticSecret.find_or_initialize_by(broker_credential: credential)
      return secret unless secret.new_record?

      secret.namespace = credential.namespace
      secret.name = "#{credential.name} token"
      secret.inject_config = { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" }
      secret.source = SecretSource.new(source_type: "token_broker", config: { "credential_id" => credential.oid })
      secret.rules = Array(@provider.api_hosts).each_with_index.map do |host, position|
        RequestRule.new(host: host, http_methods: [], paths: [], position: position)
      end
      secret.save!
      secret
    end

    def read_and_clear_flow_cookie
      raw = cookies.encrypted[FLOW_COOKIE]
      cookies.delete(FLOW_COOKIE)
      return nil if raw.blank?
      JSON.parse(raw)
    rescue JSON::ParserError
      nil
    end

    # Renders the team-facing failure page. +kind+ is :denied or :error; the
    # status defaults to 422 but callers override it for the 4xx pre-consent
    # rejections. Success does not come through here -- the happy path
    # redirects back to the console Integrations page.
    def render_result(kind, status: :unprocessable_entity, message: nil)
      @kind = kind
      @message = message
      render :result, status: status
    end
  end
end
