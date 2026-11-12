# The credential scheme of the console API (/api/v1/console/*, the browser
# SPA). Include into Api::V1::Console::BaseController, or into a thin
# Api::V1::Console subclass of a bearer Api::V1 controller to serve the same
# resource to the SPA, the way SandboxAdminAuthentication serves it to a
# sandbox.
#
# Session only: the console session cookie, never a bearer key.
#
# - A request with an Authorization header gets 403
#   details.reason=session_required (it never falls back to the cookie).
# - No session: 401 details.reason=session_required.
# - A disabled session user is signed out on every endpoint and gets 401
#   details.reason=account_disabled; a pending one gets 403 account_pending
#   unless the action allows pending users.
# - Writes (anything but GET/HEAD) need the session's CSRF token in
#   X-CSRF-Token (from GET /api/v1/console/me) and a same-origin Origin;
#   otherwise 403 details.reason=csrf.
# - Every action is admin-only (403 admin_required) unless the controller opts
#   in with `allow_active_users` (scope records with `owned_scope` /
#   `require_owner_or_admin!`) or `allow_pending_users`.
#
# Only controllers in the Api::V1::Console namespace may include this; any
# other controller fails closed.
module ConsoleApiAuthentication
  extend ActiveSupport::Concern
  include ActionController::RequestForgeryProtection

  CSRF_SAFE_METHODS = %w[GET HEAD].freeze

  included do
    # The railtie only applies forgery-protection defaults to
    # ActionController::Base; share its Origin-header check with the API.
    self.forgery_protection_origin_check = ActionController::Base.forgery_protection_origin_check

    skip_before_action :authenticate_api_key!
    before_action :authenticate_console_session!
    before_action :require_active_account!
    before_action :verify_session_csrf_token!
    before_action :require_admin!
  end

  class_methods do
    def accepted_credential
      :session
    end

    # Opts actions into any active (approved, not disabled) account instead
    # of the admin-only default. The action must scope what it loads to the
    # caller unless acting_admin?.
    def allow_active_users(**options)
      skip_before_action :require_admin!, **options
    end

    # Opts actions into pending accounts as well. Implies allow_active_users.
    def allow_pending_users(**options)
      skip_before_action :require_admin!, **options
      skip_before_action :require_active_account!, **options
    end
  end

  def current_user
    console_session_user if session_authenticated?
  end

  private

  def session_authenticated?
    @console_session_authenticated == true
  end

  def acting_admin?
    current_user&.admin? || false
  end

  def authenticate_console_session!
    return refuse_credential_namespace_mismatch! unless console_namespace_request?

    if request.headers["Authorization"].present?
      return render_error(status: :forbidden,
                          message: "the console API requires a console session; API keys are not accepted " \
                                   "(the bearer API is /api/v1 outside /api/v1/console)",
                          details: { reason: "session_required" })
    end
    unless console_session_user
      return render_error(status: :unauthorized, message: "console session required",
                          details: { reason: "session_required" })
    end
    # The flash survives the reset so the login page explains why.
    if console_session_user.disabled?
      reset_session
      request.flash[:alert] = ConsoleSignIn::DISABLED_MESSAGE
      return render_error(status: :unauthorized, message: "account disabled",
                          details: { reason: "account_disabled" })
    end

    @console_session_authenticated = true
  end

  # Only the console namespace takes the session; see Api::BaseController.
  def console_namespace_request?
    controller_path.start_with?(Api::BaseController::CONSOLE_CONTROLLER_PREFIX) &&
      request.path.start_with?(Api::BaseController::CONSOLE_PATH_PREFIX)
  end

  # A pending session user gets 403 so the SPA can show the holding page.
  def require_active_account!
    return if console_session_user.active?

    render_error(status: :forbidden, message: "account pending approval", details: { reason: "account_pending" })
  end

  # Writes need the session's CSRF token and an Origin header naming this
  # console; an absent or "null" Origin is refused.
  def verify_session_csrf_token!
    return if CSRF_SAFE_METHODS.include?(request.request_method)
    return if request.origin.present? && request.origin == request.base_url && any_authenticity_token_valid?

    render_error(status: :forbidden, message: "invalid or missing CSRF token", details: { reason: "csrf" })
  end

  def require_admin!
    return if acting_admin?

    render_error(status: :forbidden, message: "admin access required", details: { reason: "admin_required" })
  end

  # One-shot handoff to the SPA of the flash set by a redirect-based flow
  # (SSO or OAuth callback).
  def consume_console_flash
    flash = request.flash
    payload = { notice: flash[:notice].presence, alert: flash[:alert].presence }.compact
    flash.discard
    payload
  end

  # Records an active-user endpoint may expose: everything for an acting
  # admin, otherwise only rows whose +owner+ association/column is the caller.
  def owned_scope(scope, owner: :user)
    acting_admin? ? scope : scope.where(owner => current_user)
  end

  # Guard for a single record loaded by an active-user endpoint. Renders 404
  # (not 403) so record existence is not disclosed to non-owners.
  def require_owner_or_admin!(record, owner: :user)
    return true if acting_admin? || record.public_send(owner) == current_user

    render_error(status: :not_found, message: "not found")
    false
  end
end
