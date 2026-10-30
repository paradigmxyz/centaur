class SecretSource < ApplicationRecord
  oid_prefix "scs"

  include SyncConfigOwnerInvalidation

  SOURCE_TYPES = %w[env aws_sm aws_ssm 1password 1password_connect control_plane token_broker].freeze
  SYNC_CONFIG_REPLACEMENT_ATTRIBUTES = %w[source_type config secret role role_kind broker_credential_id].freeze

  UNIVERSAL_OPTIONAL = %w[json_key ttl].freeze

  CONFIG_SCHEMA = {
    "env" => { required: %w[var], optional: [] },
    "aws_sm" => { required: %w[secret_id], optional: %w[region] },
    "aws_ssm" => { required: %w[name], optional: %w[region with_decryption] },
    "1password" => { required: %w[secret_ref], optional: %w[token_env] },
    "1password_connect" => { required: %w[secret_ref], optional: %w[host_env token_env] },
    "control_plane" => { required: [], optional: [] },
    "token_broker" => { required: %w[credential_id], optional: [] }
  }.freeze

  # A source belongs to exactly one owner. static_secret feeds the `secrets`
  # transform; gcp_auth_secret is a gcp_auth keyfile; oauth_token_secret holds
  # one oauth_token entry's credential fields and token-endpoint headers;
  # gcp_id_token_secret is a gcp_id_token keyfile;
  # pg_dsn_secret is a Postgres upstream's connection string; hmac_secret holds
  # one hmac_sign entry's HMAC key and any additional named credentials.
  belongs_to :static_secret, optional: true
  belongs_to :gcp_auth_secret, optional: true
  belongs_to :gcp_id_token_secret, optional: true
  belongs_to :aws_auth_secret, optional: true
  belongs_to :oauth_token_secret, optional: true
  belongs_to :pg_dsn_secret, optional: true
  belongs_to :hmac_secret, optional: true
  belongs_to :broker_credential, optional: true

  # Only set for oauth_token_secret- and hmac_secret-owned sources: whether
  # `role` names a credential field (client_id, secret, ...) or a token-endpoint
  # header.
  enum :role_kind, { credential_field: "credential_field", endpoint_header: "endpoint_header" }, validate: { allow_nil: true }

  encrypts :secret

  attr_readonly :source_type
  before_validation :resolve_broker_credential_reference

  # Resolve either assignment order before sync replacement comparisons inspect
  # broker_credential_id on an unsaved candidate source.
  def source_type=(value)
    super
    resolve_broker_credential_reference
  end

  def config=(value)
    super
    resolve_broker_credential_reference
  end

  # Maps this source to the iron-proxy `secrets` transform `source` block,
  # discriminated by `type`. For control_plane sources the decrypted value is
  # delivered inline; all other types pass their config through (the proxy's
  # backend resolvers read the matching keys and ignore unknown ones).
  #
  # A token_broker source is resolved server-side: control mints and rotates the
  # access token, so it is delivered inline exactly like control_plane (the proxy
  # injects it directly, and Principal.redact_live_secrets redacts it, with no
  # special handling for either). The credential reference never reaches the proxy.
  def to_proxy_source
    return { "type" => "control_plane", "value" => brokered_credential&.access_token } if source_type == "token_broker"

    source = config.is_a?(Hash) ? config.dup : {}
    source["type"] = source_type
    source["value"] = secret if source_type == "control_plane"
    source
  end

  # Whether this source can currently deliver a value to a proxy. Always true
  # except for a token_broker source whose credential has not minted an access
  # token yet (bootstrapping) or is dead -- those are omitted from sync so the
  # proxy never receives an empty inline value.
  def deliverable?
    return brokered_credential&.access_token.present? if source_type == "token_broker"
    true
  end

  # Whether the value this source delivers resolves from ``credential``. Lets a
  # caller verify that a wrapper's linked credential is the one actually served.
  def resolves_credential?(credential)
    credential.present? && brokered_credential&.id == credential.id
  end

  # Used to invalidate principals when a broker token rotates and to block
  # deleting a credential that a token_broker source still references.
  def self.referencing_broker_credential(credential)
    where(source_type: "token_broker", broker_credential_id: credential.id)
  end

  OWNER_ASSOCIATIONS = %i[
    static_secret gcp_auth_secret gcp_id_token_secret aws_auth_secret oauth_token_secret pg_dsn_secret hmac_secret
  ].freeze

  # Owners whose sources fill a named role (credential field or endpoint header).
  # aws_auth's sources are credential fields (access_key_id, secret_access_key,
  # session_token), like hmac/oauth_token.
  ROLE_OWNERS = %i[oauth_token_secret hmac_secret aws_auth_secret].freeze

  validates :source_type, presence: true, inclusion: { in: SOURCE_TYPES }
  validate :config_is_a_hash
  validate :config_matches_source_type
  validate :secret_matches_source_type
  validate :at_most_one_owner
  validate :role_matches_owner
  validate :token_broker_reference_resolves

  private

  # The BrokerCredential a token_broker source references. credential_id is either
  # an opaque id (bcr_...) or a globally unique foreign_id. Returns nil when the
  # source is not a token_broker, the reference is incomplete, or nothing matches.
  def brokered_credential
    resolve_broker_credential_reference if broker_credential_id.nil? || will_save_change_to_config?
    broker_credential
  end

  def resolve_broker_credential_reference
    unless source_type == "token_broker" && config.is_a?(Hash)
      self.broker_credential = nil
      return
    end

    ref = config["credential_id"]
    if ref.blank?
      self.broker_credential = nil
      return
    end

    self.broker_credential = if BrokerCredential.decode_oid(ref)
      BrokerCredential.find_by_oid(ref)
    else
      BrokerCredential.find_by(foreign_id: ref)
    end
  end

  # A token_broker source must point at a real credential.
  def token_broker_reference_resolves
    return unless source_type == "token_broker" && config.is_a?(Hash)
    ref = config["credential_id"]
    return if ref.blank? # missing-key reported by config_matches_source_type

    if brokered_credential.nil?
      errors.add(:config, "credential_id #{ref.inspect} does not reference an existing broker credential")
    end
  end

  def at_most_one_owner
    # Check the association object, not just the FK column: when built through a
    # parent (parent.sources.build / parent.keyfile_source =) autosave validates
    # this record before the parent is persisted, so the FK is still nil but the
    # inverse association is already set.
    set = OWNER_ASSOCIATIONS.count { |assoc| send(assoc).present? }
    return if set <= 1
    errors.add(:base, "must belong to at most one of #{OWNER_ASSOCIATIONS.join(", ")}")
  end

  def role_matches_owner
    if ROLE_OWNERS.any? { |assoc| send(assoc).present? }
      errors.add(:role, "can't be blank for a #{ROLE_OWNERS.join(" or ")} source") if role.blank?
      errors.add(:role_kind, "can't be blank for a #{ROLE_OWNERS.join(" or ")} source") if role_kind.blank?
    else
      errors.add(:role, "is only allowed for a #{ROLE_OWNERS.join(" or ")} source") if role.present?
      errors.add(:role_kind, "is only allowed for a #{ROLE_OWNERS.join(" or ")} source") if role_kind.present?
    end
  end

  def config_is_a_hash
    errors.add(:config, "must be a hash") unless config.is_a?(Hash)
  end

  def config_matches_source_type
    return unless config.is_a?(Hash)
    schema = CONFIG_SCHEMA[source_type]
    return unless schema

    keys = config.keys.map(&:to_s)
    allowed = schema[:required] + schema[:optional] + UNIVERSAL_OPTIONAL

    (schema[:required] - keys).each do |missing|
      errors.add(:config, "is missing required key #{missing.inspect} for source_type #{source_type.inspect}")
    end

    (keys - allowed).each do |unknown|
      errors.add(:config, "has unknown key #{unknown.inspect} for source_type #{source_type.inspect}")
    end
  end

  def secret_matches_source_type
    if source_type == "control_plane"
      errors.add(:secret, "can't be blank for source_type \"control_plane\"") if secret.blank?
    elsif secret.present?
      errors.add(:secret, "is only allowed for source_type \"control_plane\"")
    end
  end
end
