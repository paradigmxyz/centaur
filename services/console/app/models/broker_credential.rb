# A managed OAuth credential. centaur-token-broker refreshes its access token
# before expiry, working directly on this table.
#
# The minted access token reaches iron-proxy through the normal /sync path: a
# `token_broker` SecretSource on some grantable secret references this credential
# by `credential_id` (its oid), and SecretSource#to_proxy_source resolves it to
# the current access token, delivered inline like a control_plane value. A
# BrokerCredential is itself NOT synced and NOT grantable.
#
# The OAuth credentials it refreshes with -- client_id, optional client_secret,
# optional username/password/api_key, and any token-endpoint headers -- are fields
# on the credential. client_id is not secret; every
# other credential value is encrypted at rest.
class BrokerCredential < ApplicationRecord
  oid_prefix "bcr"

  include ForeignIdCollisionGuard

  URL_SAFE_FORMAT = /\A[A-Za-z0-9\-._~]+\z/
  URL_SAFE_MESSAGE = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"

  PREQIN_TOKEN_ENDPOINT = "https://api.preqin.com/connect/token".freeze
  GRANTS = %w[refresh_token client_credentials password preqin].freeze

  # The access token must keep at least this much life past the scheduled
  # refresh, regardless of slack/fraction. Matches centaur-token-broker.
  REFRESH_FLOOR_SECONDS = 60
  # IdPs that omit expires_in get a conservative default so the token is
  # refreshed before it quietly stops working. Matches centaur-token-broker.
  DEFAULT_EXPIRES_IN_SECONDS = 5 * 60

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

  # Refuse to delete a credential that token_broker sources still reference: there
  # is no FK to cascade or nullify, so deletion would silently leave those secrets
  # undeliverable. The operator must remove the references first.
  before_destroy :ensure_not_referenced
  after_commit :auto_grant_matching_principals, on: %i[create update], if: :oauth_app_id?
  before_validation :default_preqin_token_endpoint
  before_commit :bump_referencing_principal_sync_config_versions, if: :sync_config_relevant_change?

  serialize :token_endpoint_headers, coder: JSON
  encrypts :access_token
  encrypts :refresh_token
  encrypts :client_secret
  encrypts :username
  encrypts :password
  encrypts :api_key
  encrypts :token_endpoint_headers

  validates :grant, inclusion: { in: GRANTS, message: "must be one of #{GRANTS.join(", ")}" }
  validates :foreign_id, uniqueness: { allow_nil: true },
            format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }, allow_nil: true
  validate :foreign_id_cannot_change, on: :update
  validates :token_endpoint, presence: true
  # client_id is sourced from the linked OauthApp for flow-minted credentials, so
  # it is only required for standalone grants whose strategy uses it.
  validates :client_id, presence: true, if: -> { grant != "preqin" && !oauth_app_id? }
  validates :external_user_key, format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE },
            length: { maximum: 128 }, allow_nil: true
  validates :early_refresh_fraction,
            numericality: { greater_than_or_equal_to: 0, less_than: 1 }
  validates :early_refresh_slack_seconds, :max_refresh_interval_seconds, :refresh_timeout_seconds,
            numericality: { only_integer: true, greater_than: 0 }
  validate :labels_is_a_hash
  validate :scopes_is_an_array
  validate :grant_credentials_present
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

  def preqin? = grant == "preqin"

  # The wall-clock time centaur-token-broker should next refresh. min(early-trigger,
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

  private

  def auto_grant_matching_principals
    PrincipalCredentialReconciliation.new.apply_for_credential(self)
  end

  def foreign_id_cannot_change
    return unless foreign_id_in_database.present? && will_save_change_to_foreign_id?

    errors.add(:foreign_id, "cannot be changed once set")
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
    scopes = SecretSource.referencing_broker_credential(self).filter_map do |source|
      owner = source.sync_config_owner
      Principal.effective_grantees_for_grantable(owner) if owner
    end
    Principal.bump_sync_config_cache_versions(Principal.combine_scopes(scopes))
  end

  def labels_is_a_hash
    errors.add(:labels, "must be a hash") unless labels.is_a?(Hash)
  end

  def scopes_is_an_array
    return if scopes.is_a?(Array) && scopes.all?(String)
    errors.add(:scopes, "must be an array of strings")
  end

  def grant_credentials_present
    case grant
    when "client_credentials"
      errors.add(:client_secret, "can't be blank for the client_credentials grant") if effective_client_secret.blank?
    when "password"
      errors.add(:username, "can't be blank for the password grant") if username.blank?
      errors.add(:password, "can't be blank for the password grant") if password.blank?
    when "preqin"
      errors.add(:username, "can't be blank for the Preqin broker grant") if username.blank?
      errors.add(:api_key, "can't be blank for the Preqin broker grant") if api_key.blank?
    end
  end

  def token_endpoint_headers_valid
    return if token_endpoint_headers.nil?
    valid = token_endpoint_headers.is_a?(Hash) &&
            token_endpoint_headers.all? { |k, v| k.is_a?(String) && v.is_a?(String) }
    errors.add(:token_endpoint_headers, "must be an object mapping header names to string values") unless valid
  end

  def default_preqin_token_endpoint
    return unless grant == "preqin"

    self.token_endpoint = PREQIN_TOKEN_ENDPOINT
  end
end
