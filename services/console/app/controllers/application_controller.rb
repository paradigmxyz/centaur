class ApplicationController < ActionController::Base
  include ConsoleSession
  include ConsoleSignIn

  # Only allow modern browsers supporting webp images, web push, badges, import maps, CSS nesting, and CSS :has.
  allow_browser versions: :modern

  # Changes to the importmap will invalidate the etag for HTML responses
  stale_when_importmap_changes

  # UI-wide 404 for record lookups (find_by_oid! and friends), so console
  # controllers don't each hand-roll a rescue. Mirrors Api::BaseController.
  rescue_from ActiveRecord::RecordNotFound, with: :render_not_found

  helper_method :current_user, :acting_admin?, :descoped?
  helper_method :public_base_url, :oauth_callback_redirect_uri

  # The public origin the console is reached at. Derived from the request by
  # default; CENTAUR_CONSOLE_PUBLIC_URL overrides it for deployments behind
  # proxies whose Host header doesn't match the public origin. Shared by the
  # OAuth flow controller (the redirect URI it sends the IdP) and the console
  # (the redirect URI / start-URL template it shows operators), so the two never
  # drift.
  def public_base_url
    ConsoleEnv["PUBLIC_URL"].presence || request.base_url
  end

  # The OAuth callback redirect URI registered with the IdP for an app:
  # "<public base>/oauth/<slug>/callback". One per app, keyed by its slug.
  def oauth_callback_redirect_uri(slug)
    URI.join(public_base_url, "/oauth/#{slug}/callback").to_s
  end

  # Gate every UI route behind a console session by default. Controllers that
  # must stay reachable while signed out (e.g. the login form) skip this. API
  # controllers descend from ActionController::API, not this class, so they keep
  # their own ApiKey/proxy-token auth and are unaffected.
  before_action :require_login
  # A signed-in user must also be approved (active) to use the console. The login
  # and pending controllers skip this so pending users can reach the holding page
  # and sign out.
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

  # before_action gate for console pages: bounce anonymous requests to the login
  # form rather than rendering the page.
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

  # Guard for admin-only controllers (the Control section and user
  # management). Not a global gate. Bounces non-admins to their default section.
  # Keep this redirect silent: direct/admin-default URLs are not
  # actionable errors for non-admin operators, especially on a fresh visit.
  def require_admin
    redirect_to default_console_landing_path unless acting_admin?
  end

  # Where a signed-in user lands when no explicit destination applies: admins get
  # the Control section and everyone else gets Integrations.
  def default_console_landing_path
    acting_admin? ? console_principals_path : console_integrations_path
  end

  # Completes an SSO login: establishes the console session and redirects to
  # the remembered return path or the user's landing page. The SPA login page
  # shows the flash (notice or alert) through GET /api/v1/auth/config.
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
