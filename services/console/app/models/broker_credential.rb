# A managed OAuth credential whose token lifecycle centaur-console owns itself:
# the in-control port of iron-token-broker. control drives a refresh loop
# (Broker::PollRefreshJob -> Broker::RefreshCredentialJob -> #refresh!) that
# mints fresh access tokens before expiry.
#
# The minted access token reaches iron-proxy through the normal /sync path: a
# `token_broker` SecretSource on some grantable secret references this credential
# by `credential_id` (its oid), and SecretSource#to_proxy_source resolves it to
# the current access token, delivered inline like a control_plane value. A
# BrokerCredential is itself NOT synced and NOT grantable.
#
# The OAuth credentials it refreshes with -- client_id, optional client_secret,
# optional username/password, and any token-endpoint headers -- are fields on the
# credential, resolved by control itself. client_id is not secret; every other
# credential value is encrypted at rest.
class BrokerCredential < ApplicationRecord
  oid_prefix "bcr"

  include ForeignIdCollisionGuard

  URL_SAFE_FORMAT = /\A[A-Za-z0-9\-._~]+\z/
  URL_SAFE_MESSAGE = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"

  GRANTS = %w[refresh_token password].freeze

  # The access token must keep at least this much life past the scheduled
  # refresh, regardless of slack/fraction. Mirrors the 60s floor in
  # iron-token-broker's nextRefreshAt.
  REFRESH_FLOOR_SECONDS = 60
  # IdPs that omit expires_in get a conservative default so the loop refreshes
  # before the token quietly stops working. Mirrors refreshOnce.
  DEFAULT_EXPIRES_IN_SECONDS = 5 * 60
  # Exponential backoff for retryable failures: base 5s, doubling, capped at 5m.
  BACKOFF_BASE_SECONDS = 5
  BACKOFF_MAX_SECONDS = 5 * 60

  # Optional: a flow-minted credential has no console operator behind it (the
  # public consent flow runs unauthenticated).
  belongs_to :created_by, class_name: "User", optional: true
  # Set on credentials minted by the OAuth consent flow; they delegate their
  # client_id/client_secret to the app (see #effective_client_secret).
  belongs_to :oauth_app, optional: true
  # The grantable static secret wrapping this credential (the OAuth consent flow
  # auto-creates one; at most one, enforced by a unique index). Nullify on delete --
  # the before_destroy guard below already blocks deletion while a token_broker
  # source still references the credential.
  has_one :static_secret, dependent: :nullify

  attr_writer :refresh_client

  # Refuse to delete a credential that token_broker sources still reference: there
  # is no FK to cascade or nullify, so deletion would silently leave those secrets
  # undeliverable. The operator must remove the references first.
  before_destroy :ensure_not_referenced
  before_commit :bump_referencing_principal_sync_config_versions, if: :sync_config_relevant_change?

  serialize :token_endpoint_headers, coder: JSON
  encrypts :access_token
  encrypts :refresh_token
  encrypts :client_secret
  encrypts :username
  encrypts :password
  encrypts :token_endpoint_headers

  scope :refreshable, -> {
    where(dead: false)
      .where("(\"broker_credentials\".\"grant\" = ? AND " \
             "(last_refresh IS NULL OR refresh_token IS NOT NULL)) OR " \
             "\"broker_credentials\".\"grant\" = ?",
             "refresh_token", "password")
      .where("next_attempt_at IS NULL OR next_attempt_at <= ?", Time.current)
  }

  validates :grant, inclusion: { in: GRANTS, message: "must be one of #{GRANTS.join(", ")}" }
  validates :namespace, presence: true, format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }
  validates :foreign_id, uniqueness: { scope: :namespace, allow_nil: true },
            format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }, allow_nil: true
  validates :token_endpoint, presence: true
  # client_id is sourced from the linked OauthApp for flow-minted credentials, so
  # it is only required on standalone (operator-authored) credentials.
  validates :client_id, presence: true, unless: :oauth_app_id?
  validates :external_user_key, format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE },
            length: { maximum: 128 }, allow_nil: true
  validates :early_refresh_fraction,
            numericality: { greater_than_or_equal_to: 0, less_than: 1 }
  validates :early_refresh_slack_seconds, :max_refresh_interval_seconds, :refresh_timeout_seconds,
            numericality: { only_integer: true, greater_than: 0 }
  validate :labels_is_a_hash
  validate :scopes_is_an_array
  validate :password_grant_credentials_present
  validate :token_endpoint_headers_valid

  # OAuth client identity used for refresh. Flow-minted credentials delegate to
  # their OauthApp so a client-secret rotation on the app applies to every
  # credential it minted; standalone credentials use their own columns.
  def effective_client_id     = oauth_app&.client_id || client_id
  def effective_client_secret = oauth_app ? oauth_app.client_secret : client_secret

  # bootstrapping: seeded but never refreshed; dead: needs human re-auth;
  # live: has minted at least one access token.
  def status
    return "dead" if dead?
    return "bootstrapping" if last_refresh.nil?
    "live"
  end

  # --- Refresh state machine (ported from iron-token-broker credential.go) ----

  # The wall-clock time the loop should next refresh. min(early-trigger,
  # max-interval-ceiling) with a 60s floor before expiry. A credential that has
  # never refreshed (no blob) is due immediately.
  def compute_next_attempt_at(now: Time.current)
    return now if expires_at.nil? || last_refresh.nil?

    slack = early_refresh_slack_seconds
    ttl = expires_at - last_refresh
    if early_refresh_fraction.positive? && ttl.positive?
      frac_slack = ttl * early_refresh_fraction
      slack = frac_slack if frac_slack > slack
    end
    slack = REFRESH_FLOOR_SECONDS if slack < REFRESH_FLOOR_SECONDS

    early = expires_at - slack
    ceiling = last_refresh + max_refresh_interval_seconds
    [ early, ceiling ].min
  end

  # Performs one refresh attempt under a row lock (the single-writer guarantee:
  # concurrent refresh attempts serialize on the same row, so the refresh family
  # is never used twice concurrently). Persists the outcome --
  # success advances the blob + schedules the next refresh, a retryable failure
  # schedules a backoff retry, and an unrecoverable failure marks the credential
  # dead. Never raises for an IdP/config failure; the state is in the row.
  def refresh!(now: Time.current)
    with_lock do
      return if dead?

      result, clear_refresh_token = perform_refresh
      apply_success!(result, now: now, clear_refresh_token: clear_refresh_token) if result
    rescue Broker::RefreshError => e
      if e.retryable?
        record_retryable_failure(e.message, now: now)
      else
        mark_dead!(e.reason)
      end
    end
  end

  private

  def refresh_client
    @refresh_client ||= Broker::RefreshClient.new
  end

  def perform_refresh
    case grant
    when "password"
      perform_password_grant_refresh
    else
      perform_refresh_token_refresh
    end
  end

  def perform_refresh_token_refresh
    if refresh_token.blank?
      mark_dead!("missing_initial_refresh_token")
      return nil
    end

    [ refresh_with_stored_token, false ]
  end

  def refresh_with_stored_token
    refresh_client.refresh(
      token_endpoint: token_endpoint,
      grant: "refresh_token",
      client_id: effective_client_id,
      client_secret: effective_client_secret,
      refresh_token: refresh_token,
      scopes: refresh_scopes_for_provider,
      headers: token_endpoint_headers || {},
      timeout: refresh_timeout_seconds
    )
  end

  def perform_password_grant_refresh
    clear_stale_refresh_token = false

    if refresh_token.present?
      begin
        return [ refresh_with_stored_token, false ]
      rescue Broker::RefreshError => e
        raise if e.retryable?

        Rails.logger.warn do
          "broker credential #{oid} refresh_token grant failed with #{e.reason}; " \
            "falling back to password grant"
        end
        clear_stale_refresh_token = true
      end
    end

    unless password_grant_initial_values_present?
      mark_dead!("password_grant_missing_initial_values")
      return nil
    end

    result = refresh_client.refresh(
      token_endpoint: token_endpoint,
      grant: "password",
      client_id: effective_client_id,
      client_secret: effective_client_secret,
      username: username,
      password: password,
      scopes: refresh_scopes_for_provider,
      headers: token_endpoint_headers || {},
      timeout: refresh_timeout_seconds
    )
    [ result, clear_stale_refresh_token && result.refresh_token.blank? ]
  end

  def refresh_scopes_for_provider
    oauth_app&.provider_strategy&.refresh_scopes(scopes) || scopes
  end

  def apply_success!(result, now:, clear_refresh_token:)
    expires_in = result.expires_in&.positive? ? result.expires_in : DEFAULT_EXPIRES_IN_SECONDS
    attrs = {
      access_token: result.access_token,
      expires_at: now + expires_in,
      last_refresh: now,
      failure_count: 0,
      dead: false,
      dead_reason: nil
    }
    # Carry the previous refresh_token forward when the IdP did not rotate.
    if result.refresh_token.present?
      attrs[:refresh_token] = result.refresh_token
    elsif clear_refresh_token
      attrs[:refresh_token] = nil
    end
    assign_attributes(attrs)
    self.next_attempt_at = compute_next_attempt_at(now: now)
    save!
    Rails.logger.info { "broker credential #{oid} refreshed; expires_at=#{expires_at.iso8601}" }
  end

  def record_retryable_failure(reason, now:)
    self.failure_count += 1
    self.next_attempt_at = now + backoff_delay(failure_count)
    save!
    Rails.logger.warn { "broker credential #{oid} refresh failed (retryable, attempt #{failure_count}): #{reason}" }
  end

  def mark_dead!(reason)
    assign_attributes(dead: true, dead_reason: reason)
    save!(validate: false)
    Rails.logger.error { "broker credential #{oid} marked dead; human re-auth required: reason=#{reason}" }
  end

  def backoff_delay(attempt)
    exp = BACKOFF_BASE_SECONDS * (2**[ attempt - 1, 6 ].min)
    [ exp, BACKOFF_MAX_SECONDS ].min
  end

  def ensure_not_referenced
    return unless SecretSource.referencing_broker_credential(self).exists?
    errors.add(:base, "is referenced by one or more token_broker secret sources; remove those references first")
    throw :abort
  end

  def sync_config_relevant_change?
    previous_changes.key?("access_token") ||
      previous_changes.key?("dead") ||
      previous_changes.key?("last_refresh") ||
      previous_changes.key?("expires_at")
  end

  def bump_referencing_principal_sync_config_versions
    ids = SecretSource.referencing_broker_credential(self).flat_map do |source|
      owner = source.sync_config_owner
      owner ? Principal.effective_grantee_ids_for_grantable(owner) : []
    end
    Principal.bump_sync_config_cache_versions(ids)
  end

  def labels_is_a_hash
    errors.add(:labels, "must be a hash") unless labels.is_a?(Hash)
  end

  def scopes_is_an_array
    return if scopes.is_a?(Array) && scopes.all?(String)
    errors.add(:scopes, "must be an array of strings")
  end

  def password_grant_credentials_present
    return unless grant == "password"

    errors.add(:username, "can't be blank for the password grant") if username.blank?
    errors.add(:password, "can't be blank for the password grant") if password.blank?
  end

  def password_grant_initial_values_present?
    username.present? && password.present?
  end

  def token_endpoint_headers_valid
    return if token_endpoint_headers.nil?
    valid = token_endpoint_headers.is_a?(Hash) &&
            token_endpoint_headers.all? { |k, v| k.is_a?(String) && v.is_a?(String) }
    errors.add(:token_endpoint_headers, "must be an object mapping header names to string values") unless valid
  end
end
