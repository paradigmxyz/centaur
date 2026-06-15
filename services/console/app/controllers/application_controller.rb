class ApplicationController < ActionController::Base
  # Only allow modern browsers supporting webp images, web push, badges, import maps, CSS nesting, and CSS :has.
  allow_browser versions: :modern

  # Changes to the importmap will invalidate the etag for HTML responses
  stale_when_importmap_changes

  # UI-wide 404 for record lookups (find_by_oid! and friends), so console
  # controllers don't each hand-roll a rescue. Mirrors Api::BaseController.
  rescue_from ActiveRecord::RecordNotFound, with: :render_not_found

  helper_method :current_user
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

  # The signed-in operator for cookie-session (console) requests, or nil. Distinct
  # from Api::BaseController#current_user, which resolves a User from an API key.
  def current_user
    @current_user ||= User.find_by(id: session[:user_id]) if session[:user_id]
  end

  # before_action gate for console pages: bounce anonymous requests to the login
  # form rather than rendering the page.
  def require_login
    redirect_to login_path unless current_user
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

  # Guard for admin-only controllers (e.g. user management). Not a global gate.
  def require_admin
    redirect_to root_path, alert: "That page is restricted to admins." unless current_user&.admin?
  end

  # Establishes the console cookie session and sends the user to the right
  # post-login page. Password login re-renders for disabled accounts; SSO login
  # redirects because it is returning from an external provider.
  def sign_in_console_user(user, disabled: :redirect)
    if user.disabled?
      if disabled == :render
        flash.now[:alert] = "Your account is disabled."
        return render :new, status: :unprocessable_entity
      end

      return redirect_to login_path, alert: "Your account is disabled."
    end

    reset_session
    session[:user_id] = user.id
    if user.active?
      redirect_to console_principals_path, notice: "Signed in as #{user.email}."
    else
      redirect_to pending_path, notice: "Your account is awaiting approval."
    end
  end

  def render_not_found(e)
    render plain: e.message, status: :not_found
  end
end
