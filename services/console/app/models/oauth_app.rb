# An operator-registered OAuth application: the provider, the OAuth client
# (client_id + encrypted client_secret), and the scopes its consent flow
# requests.
#
# Each app has a globally-unique +slug+ that names its well-known consent links:
# a team member who knows the integration ("google") clicks
# /oauth/<slug>/start, consents, and lands on an centaur-console result page. The
# unauthenticated /oauth/:slug/start + /callback endpoints resolve an OauthApp by
# slug, run the provider's consent flow, and on success upsert a BrokerCredential
# linked back to this app (see BrokerCredential#oauth_app). The app is the
# durable config; its minted credentials are refreshed by the normal
# Broker::PollRefreshJob loop and delegate their client_id/client_secret back to
# the app, so rotating the app's secret fixes every credential it minted.
#
# Provider-generic by design: one model with a `provider` column and a small
# strategy registry (Oauth::Providers), not a table per provider. Today the only
# provider is Google.
class OauthApp < ApplicationRecord
  oid_prefix "oap"

  URL_SAFE_FORMAT = /\A[A-Za-z0-9\-._~]+\z/
  URL_SAFE_MESSAGE = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"

  belongs_to :created_by, class_name: "User"

  # Deleting an app with minted credentials must fail: those credentials
  # delegate their client_id/secret here and would be left unable to refresh.
  # The operator deletes/unlinks the credentials first. Mirrors
  # BrokerCredential#ensure_not_referenced.
  has_many :broker_credentials, dependent: :restrict_with_error

  encrypts :client_secret

  # The slug is the app's whole identity: globally unique and URL-safe. It both
  # addresses the app in the API (oid or slug) and names the consent links, so it
  # must not start with the opaque-id prefix or the two forms would collide.
  validates :slug, presence: true, uniqueness: true,
            format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }
  validate :slug_does_not_shadow_oid
  validates :provider, inclusion: { in: ->(_) { Oauth::Providers.keys }, message: "is not a supported provider" }
  validates :client_id, presence: true
  validates :client_secret, presence: true
  validates :credential_namespace, presence: true, format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }
  validate :labels_is_a_hash
  validate :allowed_scopes_valid

  # The provider strategy backing this app, or nil if the provider column somehow
  # holds an unknown key (the inclusion validation normally prevents that).
  def provider_strategy = Oauth::Providers.fetch(provider)

  # True when every requested scope is within the allowlist.
  def scopes_allowed?(requested) = (Array(requested) - Array(allowed_scopes)).empty?

  private

  def slug_does_not_shadow_oid
    return if slug.blank?
    reserved = "#{self.class.oid_prefix}_"
    return unless slug.start_with?(reserved)
    errors.add(:slug, "must not start with #{reserved.inspect}, which is reserved for opaque ids")
  end

  def labels_is_a_hash
    errors.add(:labels, "must be a hash") unless labels.is_a?(Hash)
  end

  def allowed_scopes_valid
    return if allowed_scopes_list_valid?
    errors.add(:allowed_scopes, "must be a non-empty array of non-blank strings")
  end

  def allowed_scopes_list_valid?
    allowed_scopes.is_a?(Array) && allowed_scopes.any? &&
      allowed_scopes.all? { |s| s.is_a?(String) && s.present? }
  end
end
