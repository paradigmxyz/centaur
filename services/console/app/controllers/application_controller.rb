class ApplicationController < ActionController::Base
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
  # The sidebar thread list is global chrome (rendered by layouts/console.html.erb
  # on every page), but populating it issues several queries against the api-rs
  # ai_v2 sessions DB, including an unindexed sequential scan + sort of the
  # sessions table. Running that in every console request blocked pages that only
  # render the empty-state list (principals, roles, secrets, ...). Instead we
  # initialize the ivars empty here and load the real list lazily via a Turbo
  # Frame (Console::ThreadsController#sidebar), so the cross-database work happens
  # once, out of band, and never blocks the primary page render.
  before_action :init_console_sidebar_threads

  CONSOLE_SIDEBAR_THREAD_LIMIT = 30
  CONSOLE_SIDEBAR_SLACK_PROVIDER = Oauth::Providers::Slack::KEY
  CONSOLE_SIDEBAR_SLACK_THREAD_OWNER_METADATA_KEYS = %w[slack_user_id actor_user_id user_id].freeze
  CONSOLE_SIDEBAR_SLACK_THREAD_TEAM_METADATA_KEYS = %w[slack_team_id team_id home_team_id].freeze
  CONSOLE_SIDEBAR_SLACK_CREDENTIAL_USER_LABEL_KEYS = %w[slack_user_id].freeze
  CONSOLE_SIDEBAR_SLACK_CREDENTIAL_EMAIL_LABEL_KEYS = %w[email slack_email].freeze
  CONSOLE_SIDEBAR_SLACK_TEAM_LABEL = "slack_team_id".freeze
  CONSOLE_SIDEBAR_THREAD_OWNER_METADATA_KEYS = %w[actor_email user_email].freeze
  ConsoleSidebarSlackThreadOwner = Struct.new(:user_id, :team_id, keyword_init: true)

  private

  # The signed-in operator for cookie-session (console) requests, or nil. Distinct
  # from Api::BaseController#current_user, which resolves a User from an API key.
  def current_user
    @current_user ||= User.find_by(id: session[:user_id]) if session[:user_id]
  end

  # Whether this admin has temporarily descoped themselves to operator
  # permissions ("view as operator"). Self-healing: the flag is dropped if the
  # user is no longer an admin, so it can never outlive the privileges it pauses.
  def descoped?
    return false unless session[:descoped]
    return true if current_user&.admin?

    session.delete(:descoped)
    false
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

  # Guard for admin-only controllers (the Control and Data Sync sections, user
  # management). Not a global gate. Bounces non-admins to their only available
  # section. Keep this redirect silent: direct/admin-default URLs are not
  # actionable errors for non-admin operators, especially on a fresh visit.
  def require_admin
    redirect_to console_threads_path unless acting_admin?
  end

  # Where a signed-in user lands when no explicit destination applies: admins get
  # the Control section, everyone else the threads view (their only section).
  def default_console_landing_path
    acting_admin? ? console_principals_path : console_threads_path
  end

  # Cheap default so every page renders the empty sidebar list without touching
  # the sessions DB. The real list is filled in by #load_console_sidebar_threads,
  # invoked only from the lazy sidebar Turbo Frame.
  def init_console_sidebar_threads
    @console_sidebar_threads = []
    @console_sidebar_latest_messages = {}
  end

  def load_console_sidebar_threads
    @console_sidebar_threads = []
    @console_sidebar_latest_messages = {}
    return unless current_user&.active?

    threads = console_sidebar_visible_thread_scope
      .recent_first
      .limit(CONSOLE_SIDEBAR_THREAD_LIMIT)
      .to_a
    threads = console_sidebar_threads_with_direct_selection(threads)
    @console_sidebar_threads = threads
    @console_sidebar_latest_messages = console_sidebar_latest_messages_for(threads.map(&:thread_key))
  rescue ActiveRecord::ActiveRecordError, PG::Error => e
    Rails.logger.debug("console_sidebar_threads_unavailable error=#{e.class}: #{e.message}")
  end

  # Establishes the console cookie session and sends the user to the right
  # post-login page. Password login re-renders for disabled accounts; SSO login
  # redirects because it is returning from an external provider.
  def sign_in_console_user(user, disabled: :redirect, destination: nil)
    if user.disabled?
      if disabled == :render
        flash.now[:alert] = "Your account is disabled."
        return render :new, status: :unprocessable_entity
      end

      return redirect_to login_path, alert: "Your account is disabled."
    end

    return_to = session[:return_to]
    reset_session
    session[:user_id] = user.id
    session[:return_to] = return_to if return_to.present?
    if user.active?
      redirect_to(destination.presence || post_login_redirect_path, notice: "Signed in as #{user.email}.")
    else
      redirect_to pending_path, notice: "Your account is awaiting approval."
    end
  end

  def post_login_redirect_path
    path = session.delete(:return_to).to_s
    return default_console_landing_path unless path.start_with?("/") && !path.start_with?("//")
    path
  end

  def safe_console_return_path(default: default_console_landing_path)
    raw = params[:return_to].presence || params[:next].presence
    return default if raw.blank?

    uri = URI.parse(raw.to_s)
    return default if uri.scheme.present? || uri.host.present?

    path = uri.path.presence
    return default unless path == "/" || path&.start_with?("/console")

    uri.to_s
  rescue URI::InvalidURIError
    default
  end

  def render_not_found(e)
    render plain: e.message, status: :not_found
  end

  def console_sidebar_visible_thread_scope
    slack_owners = console_sidebar_slack_thread_owners_for_current_user
    conditions = [
      console_sidebar_console_thread_owner_sql,
      (console_sidebar_slack_thread_owner_sql(slack_owners) if slack_owners.any?)
    ].compact

    return CentaurSession.where("1=0") if conditions.empty?

    CentaurSession.where(conditions.map { |condition| "(#{condition})" }.join(" OR "))
  end

  def console_sidebar_threads_with_direct_selection(threads)
    selected = console_sidebar_direct_selected_threads(threads)
    selected.any? ? [ *selected, *threads ] : threads
  end

  def console_sidebar_direct_selected_threads(threads)
    thread_keys = console_sidebar_selected_thread_keys - threads.map(&:thread_key)
    return [] if thread_keys.empty?

    # Resolve through the owner scope, not a raw find_by, so a directly linked
    # thread only surfaces in the sidebar when the current user started it. This
    # mirrors Console::ThreadsController#selected_session.
    console_sidebar_visible_thread_scope.where(thread_key: thread_keys).to_a
  end

  # The thread param carries up to PANEL_LIMIT comma-separated keys when the
  # split view is open; every open thread should surface and highlight.
  def console_sidebar_selected_thread_keys
    return [] unless params[:controller] == "console/threads"

    params[:thread].to_s.split(",").map(&:strip).reject(&:blank?).uniq
      .first(Console::ThreadsController::PANEL_LIMIT)
  end

  def console_sidebar_console_thread_owner_sql
    email = console_sidebar_normalize_email(current_user&.email)
    return if email.blank?

    console_source = [
      "thread_key LIKE 'console:%'",
      "metadata ->> 'platform' = 'console'",
      "metadata ->> 'source' = 'console'"
    ].join(" OR ")
    owner_clauses = CONSOLE_SIDEBAR_THREAD_OWNER_METADATA_KEYS.map do |key|
      "lower(metadata ->> #{console_sidebar_sql_quote(key)}) = #{console_sidebar_sql_quote(email)}"
    end

    "(#{console_source}) AND (#{owner_clauses.join(" OR ")})"
  end

  def console_sidebar_slack_thread_owners_for_current_user
    @console_sidebar_slack_thread_owners_for_current_user ||= begin
      subjects = console_sidebar_slack_identity_subjects_for_current_user
      emails = console_sidebar_slack_identity_emails_for_current_user

      if subjects.empty? && emails.empty?
        []
      else
        credentials = BrokerCredential
          .joins(:oauth_app)
          .includes(:oauth_app)
          .where(oauth_apps: { provider: CONSOLE_SIDEBAR_SLACK_PROVIDER })
          .where(console_sidebar_slack_oauth_credential_owner_sql(subjects: subjects, emails: emails))

        credential_owners = credentials.filter_map do |credential|
          user_id = console_sidebar_first_present(
            credential.provider_subject,
            *CONSOLE_SIDEBAR_SLACK_CREDENTIAL_USER_LABEL_KEYS.map { |key| credential.labels&.[](key) }
          )
          next if user_id.blank?

          ConsoleSidebarSlackThreadOwner.new(
            user_id: user_id,
            team_id: console_sidebar_first_present(
              credential.labels&.[](CONSOLE_SIDEBAR_SLACK_TEAM_LABEL),
              credential.oauth_app&.labels&.[](CONSOLE_SIDEBAR_SLACK_TEAM_LABEL)
            )
          )
        end

        # A Slack OIDC sign-in stores the workspace user id (U…) as the
        # identity subject — the same id slackbotv2 writes into session
        # metadata — so SSO alone owns those threads even when the user has
        # not minted a broker credential through the connect flow.
        identity_owners = subjects.map do |subject|
          ConsoleSidebarSlackThreadOwner.new(user_id: subject, team_id: nil)
        end

        (credential_owners + identity_owners)
          .uniq { |owner| [ console_sidebar_normalize_key(owner.user_id), console_sidebar_normalize_key(owner.team_id) ] }
      end
    end
  end

  def console_sidebar_slack_identity_subjects_for_current_user
    current_user.user_identities
      .where(provider: CONSOLE_SIDEBAR_SLACK_PROVIDER)
      .pluck(:subject)
      .filter_map { |value| console_sidebar_normalize_key(value) }
      .uniq
  end

  def console_sidebar_slack_identity_emails_for_current_user
    ([ current_user.email ] + current_user.user_identities.where(provider: CONSOLE_SIDEBAR_SLACK_PROVIDER).pluck(:email))
      .filter_map { |value| console_sidebar_normalize_email(value) }
      .uniq
  end

  def console_sidebar_slack_oauth_credential_owner_sql(subjects:, emails:)
    clauses = []
    if subjects.any?
      subject_values = console_sidebar_sql_list(subjects)
      clauses << "lower(broker_credentials.provider_subject) IN (#{subject_values})"
      CONSOLE_SIDEBAR_SLACK_CREDENTIAL_USER_LABEL_KEYS.each do |key|
        clauses << "lower(broker_credentials.labels ->> #{console_sidebar_sql_quote(key)}) IN (#{subject_values})"
      end
    end

    if emails.any?
      email_values = console_sidebar_sql_list(emails)
      clauses << "lower(broker_credentials.provider_email) IN (#{email_values})"
      CONSOLE_SIDEBAR_SLACK_CREDENTIAL_EMAIL_LABEL_KEYS.each do |key|
        clauses << "lower(broker_credentials.labels ->> #{console_sidebar_sql_quote(key)}) IN (#{email_values})"
      end
    end

    clauses.join(" OR ")
  end

  def console_sidebar_slack_thread_owner_sql(owners)
    slack_source = [
      "thread_key LIKE 'slack:%'",
      "metadata ->> 'platform' = 'slack'",
      "metadata ->> 'source' = 'slackbotv2'"
    ].join(" OR ")

    owner_clauses = owners.map do |owner|
      user_id = console_sidebar_normalize_key(owner.user_id)
      user_clauses = CONSOLE_SIDEBAR_SLACK_THREAD_OWNER_METADATA_KEYS.map do |key|
        "lower(metadata ->> #{console_sidebar_sql_quote(key)}) = #{console_sidebar_sql_quote(user_id)}"
      end
      owner_clause = "(#{user_clauses.join(" OR ")})"

      # Team scoping narrows the match only when the credential exposes a team;
      # see Console::ThreadsController#slack_thread_owner_sql.
      if owner.team_id.present?
        team_id = console_sidebar_normalize_key(owner.team_id)
        team_clauses = CONSOLE_SIDEBAR_SLACK_THREAD_TEAM_METADATA_KEYS.map do |key|
          "lower(metadata ->> #{console_sidebar_sql_quote(key)}) = #{console_sidebar_sql_quote(team_id)}"
        end
        team_clauses << "lower(split_part(thread_key, ':', 2)) = #{console_sidebar_sql_quote(team_id)}"
        owner_clause = "(#{owner_clause} AND (#{team_clauses.join(" OR ")}))"
      end

      owner_clause
    end

    "(#{slack_source}) AND (#{owner_clauses.join(" OR ")})"
  end

  def console_sidebar_latest_messages_for(keys)
    return {} if keys.empty?

    CentaurSessionMessage
      .where(thread_key: keys)
      .select("distinct on (thread_key) session_messages.*")
      .order(Arel.sql("thread_key, created_at desc, message_id desc"))
      .index_by(&:thread_key)
  end

  def console_sidebar_first_present(*values)
    values.find(&:present?)
  end

  def console_sidebar_normalize_key(value)
    value.to_s.strip.downcase.presence
  end

  def console_sidebar_normalize_email(value)
    value.to_s.strip.downcase.presence
  end

  def console_sidebar_sql_list(values)
    values.map { |value| console_sidebar_sql_quote(value) }.join(", ")
  end

  def console_sidebar_sql_quote(value)
    ActiveRecord::Base.connection.quote(value.to_s)
  end
end
