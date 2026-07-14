require "base64"
require "digest"
require "uri"

# Console SSO login, keyed by provider: /auth/:provider/start sends an operator to
# the IdP, and /auth/:provider/callback turns the returned code into a signed-in
# User. Structurally mirrors Oauth::FlowsController (signed state, PKCE, an
# encrypted flow cookie binding the callback to the browser that started it), but
# it produces a console session instead of a BrokerCredential.
#
# Provisioning (find_or_provision_user): a returning identity matches by
# (provider, subject); a new identity whose verified email matches an existing
# user links to that user; otherwise a new user is created -- active + admin when
# the email is on the bootstrap allowlist, pending otherwise.
#
# SECURITY: never logs codes, tokens, or response bodies -- only provider keys and
# error codes, like the Broker/Oauth subsystem. Account linking is gated on
# email_verified so an unverified IdP email can't take over an existing account.
class SessionOauthController < ApplicationController
  layout "auth"

  # The login form, the IdP redirect, and the callback must all work signed out.
  skip_before_action :require_login
  skip_before_action :require_active_account

  STATE_PURPOSE = :console_login_flow
  FLOW_TTL = 10.minutes
  FLOW_COOKIE = :console_login_flow

  # Tests swap in an AuthorizationCodeClient built around an http double, mirroring
  # Oauth::FlowsController.
  class_attribute :exchange_client_factory, default: -> { Broker::AuthorizationCodeClient.new }

  before_action :set_provider

  # GET /auth/:provider/start
  def start
    nonce = SecureRandom.urlsafe_base64(32)
    code_verifier = SecureRandom.urlsafe_base64(64)

    state = Rails.application.message_verifier(STATE_PURPOSE).generate(
      { "provider" => @key, "nonce" => nonce },
      purpose: STATE_PURPOSE, expires_in: FLOW_TTL
    )

    # :lax is required -- the callback arrives via a top-level cross-site redirect
    # from the IdP, which Lax permits for GET.
    cookies.encrypted[FLOW_COOKIE] = {
      value: { "nonce" => nonce, "code_verifier" => code_verifier }.to_json,
      expires: FLOW_TTL.from_now, httponly: true, same_site: :lax
    }

    redirect_to authorization_url(state, code_verifier), allow_other_host: true
  end

  # GET /auth/:provider/callback?code=&state=  (or ?error=)
  def callback
    state = Rails.application.message_verifier(STATE_PURPOSE).verified(params[:state], purpose: STATE_PURPOSE)
    return invalid_flow if state.nil? || state["provider"] != @key

    flow = read_and_clear_flow_cookie
    return invalid_flow if flow.nil? || flow["nonce"] != state["nonce"]

    if params[:error].present?
      return redirect_to login_path, alert: "Sign in was canceled."
    end

    result = exchange_code(params[:code], flow["code_verifier"])
    identity = @provider.identity_from(result, client_id: ConsoleAuth.client_id(@key))
    sign_in_console_user(User.link_or_provision(provider: @key, identity: identity))
  rescue Broker::ExchangeError => e
    Rails.logger.error { "console login exchange failed (#{@key}): #{e.reason}" }
    redirect_to login_path, alert: "Sign in failed. Please try again."
  rescue User::SsoEmailDomainNotAllowed
    Rails.logger.warn { "console login rejected by SSO email domain allowlist (#{@key})" }
    redirect_to login_path, alert: "That email domain is not allowed to access the console."
  rescue ActiveRecord::RecordInvalid => e
    Rails.logger.error { "console login provisioning failed: #{e.record.errors.full_messages.to_sentence}" }
    redirect_to login_path, alert: "Sign in failed while setting up your account."
  end

  private

  # Resolves the provider strategy, rejecting unknown or unconfigured providers
  # (no client credentials => no button => no flow).
  def set_provider
    @key = params[:provider].to_s
    @provider = ConsoleAuth.configured?(@key) ? Login::Providers.fetch(@key) : nil
    redirect_to login_path, alert: "That sign-in method is not available." if @provider.nil?
  end

  def authorization_url(state, code_verifier)
    challenge = Base64.urlsafe_encode64(Digest::SHA256.digest(code_verifier), padding: false)
    query = {
      "client_id" => ConsoleAuth.client_id(@key),
      "redirect_uri" => callback_redirect_uri,
      "response_type" => "code",
      "scope" => @provider.scopes.join(" "),
      "state" => state,
      "code_challenge" => challenge,
      "code_challenge_method" => "S256"
    }.merge(@provider.extra_authorization_params)

    uri = URI.parse(@provider.authorization_endpoint)
    uri.query = URI.encode_www_form(query)
    uri.to_s
  end

  def exchange_code(code, code_verifier)
    exchange_client_factory.call.exchange(
      token_endpoint: @provider.token_endpoint,
      client_id: ConsoleAuth.client_id(@key),
      client_secret: ConsoleAuth.client_secret(@key),
      code: code.to_s,
      redirect_uri: callback_redirect_uri,
      code_verifier: code_verifier.to_s,
      # Login requests no offline access, so the IdP returns no refresh token.
      require_refresh_token: false
    )
  end

  # The callback redirect URI registered with the IdP: "<public base>/auth/<provider>/callback".
  def callback_redirect_uri
    URI.join(public_base_url, "/auth/#{@key}/callback").to_s
  end

  def read_and_clear_flow_cookie
    raw = cookies.encrypted[FLOW_COOKIE]
    cookies.delete(FLOW_COOKIE)
    return nil if raw.blank?
    JSON.parse(raw)
  rescue JSON::ParserError
    nil
  end

  def invalid_flow
    redirect_to login_path, alert: "This sign-in link is invalid or expired. Start again."
  end
end
