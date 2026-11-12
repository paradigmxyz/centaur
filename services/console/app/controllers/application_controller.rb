# Base class for the console's browser-facing Rails endpoints that are not
# part of the JSON API: password login (/login, /logout, /pending), SSO login
# (/auth/:provider/*), the broker OAuth consent flow (/oauth/:slug/*), the MCP
# OAuth authorization server, and the PWA launch handler. They answer with
# redirects or JSON, except the login, pending, OAuth consent, and OAuth result
# pages, which are intentionally server-rendered (layouts/oauth). The console
# UI itself is the SPA served by SpaController.
class ApplicationController < ActionController::Base
  # Only allow modern browsers supporting webp images, web push, badges, import maps, CSS nesting, and CSS :has.
  allow_browser versions: :modern

  include ConsoleSession
  include ConsoleSignIn
  include OauthAppUrls

  # 404 for record lookups (find_by_oid! and friends). Mirrors Api::BaseController.
  rescue_from ActiveRecord::RecordNotFound, with: :render_not_found

  # Require a console session by default. Endpoints that must stay reachable
  # while signed out (the login form, SSO, MCP OAuth) skip this. API controllers descend from
  # Api::BaseController, not this class, and authenticate on their own.
  before_action :require_login
  # A signed-in user must also be approved (active) to use the console. The
  # login, SSO, and MCP OAuth controllers skip this and apply their own status
  # checks.
  before_action :require_active_account

  private

  # The signed-in operator for cookie-session (console) requests, or nil.
  # Api::BaseController resolves the same session user (or an API key owner).
  def current_user
    console_session_user
  end

  # See ConsoleSession#console_session_descoped?.
  def descoped?
    console_session_descoped?
  end

  # The permission check console gates use instead of current_user.admin?: a
  # real admin who is not currently descoped. Keeping current_user untouched
  # means audit trails and data displays still see the true account.
  def acting_admin?
    current_user&.admin? && !descoped?
  end

  # before_action gate: bounce anonymous requests to the login page,
  # remembering where they were going.
  def require_login
    return if current_user

    store_return_to_for_login
    redirect_to login_path
  end

  # Second gate, after require_login: a disabled user is signed out; a pending
  # (not-yet-approved) user is sent to the holding page. Active users pass through.
  def require_active_account
    return unless current_user
    if current_user.disabled?
      reset_session
      redirect_to login_path, alert: "Your account is disabled."
    elsif current_user.pending?
      redirect_to pending_path
    end
  end

  # Guard for admin-only endpoints (shared OAuth app consent). Not a global
  # gate. Bounces non-admins to their default section, silently.
  def require_admin
    redirect_to default_console_landing_path unless acting_admin?
  end

  # Where a signed-in user lands when no explicit destination applies: admins get
  # the Control section and everyone else gets Integrations.
  def default_console_landing_path
    acting_admin? ? console_principals_path : console_integrations_path
  end

  # Completes an SSO login: establishes the console session and redirects to
  # the remembered return path or the user's landing page. Failures land on
  # the login page, which renders the flash alert.
  def sign_in_console_user(user)
    return redirect_to login_path, alert: ConsoleSignIn::DISABLED_MESSAGE if user.disabled?

    redirect_to establish_console_session!(user)
  end

  def store_return_to_for_login
    return unless request.request_method == "GET"

    remember_console_return_path(request.fullpath)
  end

  def render_not_found(e)
    render plain: e.message, status: :not_found
  end
end
