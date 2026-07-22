class Proxy < ApplicationRecord
  oid_prefix "prx"

  TOKEN_PREFIX = "iprx_".freeze
  TOKEN_FORMAT = /\Aiprx_[0-9a-f]{64}\z/
  SANDBOX_ENTITLEMENTS_PATH_PATTERN = "/api/v1/sandbox/*".freeze

  attr_readonly :bearer_token_hash
  attr_accessor :token

  # Optional: a proxy may boot unassigned and have a principal assigned or
  # swapped later. principal_id is mutable so the assignment can change.
  belongs_to :principal, optional: true

  validates :name, presence: true
  validates :bearer_token_hash, presence: true, uniqueness: true
  validate :token_matches_format, on: :create

  before_validation :issue_token, on: :create
  before_save :stamp_principal_assignment, if: :will_save_change_to_principal_id?

  # Whether the proxy currently carries a principal (and therefore any authority).
  def assigned?
    principal_id.present?
  end

  # "assigned" or "unassigned"; surfaced to operators and to the proxy on sync.
  def status
    assigned? ? "assigned" : "unassigned"
  end

  def self.find_by_token(plaintext)
    return nil if plaintext.blank?
    find_by(bearer_token_hash: hash_token(plaintext))
  end

  def self.hash_token(plaintext)
    Digest::SHA256.hexdigest(plaintext)
  end

  # The config this proxy delivers, in the iron-proxy sync shape. The assembly
  # lives on Principal (it is a function of effective grants); an unassigned
  # proxy carries no authority and resolves to the empty config. Live secret
  # values are kept inline here because the proxy needs them to resolve.
  def sync_config
    principal&.effective_config(redact_secrets: false) || Principal::EMPTY_CONFIG
  end

  def sync_config_snapshot(sandbox_entitlements_hosts: [])
    config = principal ? PrincipalSyncConfigSnapshot.fetch_for(principal).payload : Principal::EMPTY_CONFIG
    config = with_sandbox_entitlements_secret(config, sandbox_entitlements_hosts: sandbox_entitlements_hosts)
    { config_hash: config_hash_for(config), config: config }
  end

  # Opaque, deterministic fingerprint of the base (principal-derived) config.
  # Note this is not necessarily the hash the proxy echoes on sync: the sync
  # path hashes the delivered config, which also folds in the per-proxy
  # sandbox entitlements secret when one is configured (see
  # #sync_config_snapshot).
  def config_hash
    # The principal identity and assignment time are folded in so that any
    # assignment change forces a refresh, even a swap between principals whose
    # effective secrets happen to be identical (or an unassign to empty).
    config_hash_for(sync_config)
  end

  def config_hash_for(config)
    payload = config.merge(
      "principal" => principal&.oid,
      "principal_assigned_at" => principal_assigned_at&.utc&.iso8601
    )
    "sha256:#{Digest::SHA256.hexdigest(self.class.canonical_json(payload))}"
  end

  # Deep key-sorted JSON so the hash is stable regardless of Hash insertion or
  # jsonb column ordering.
  def self.canonical_json(value)
    JSON.generate(canonicalize(value))
  end

  def self.canonicalize(value)
    case value
    when Hash
      value.sort_by { |k, _| k.to_s }.to_h.transform_values { |v| canonicalize(v) }
    when Array
      value.map { |v| canonicalize(v) }
    else
      value
    end
  end

  def sandbox_entitlements_secret(hosts:)
    rules = Principal.normalize_hosts(hosts)
      .map do |host|
        {
          "host" => host,
          "methods" => [ "GET" ],
          "paths" => [ SANDBOX_ENTITLEMENTS_PATH_PATTERN ]
        }
      end
    return nil if rules.empty?

    token = SandboxEntitlements::Jwt.encode_for_proxy(self)
    return nil if token.blank?

    {
      "source" => { "type" => "control_plane", "value" => token },
      "inject" => { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
      "rules" => rules
    }
  end

  private

  def with_sandbox_entitlements_secret(config, sandbox_entitlements_hosts:)
    secret = sandbox_entitlements_secret(hosts: sandbox_entitlements_hosts)
    return config unless secret

    config.deep_dup.tap do |copy|
      copy["secrets"] = Array(copy["secrets"]) + [ secret ]
    end
  end

  # Stamp (or clear) the assignment time whenever principal_id changes, so the
  # column always reflects the current assignment.
  def stamp_principal_assignment
    self.principal_assigned_at = principal_id ? Time.current : nil
  end

  def issue_token
    return if bearer_token_hash.present?
    self.token = "#{TOKEN_PREFIX}#{SecureRandom.hex(32)}"
    self.bearer_token_hash = self.class.hash_token(token)
  end

  def token_matches_format
    return if token.blank?
    return if token.match?(TOKEN_FORMAT)
    errors.add(:token, "must match #{TOKEN_FORMAT.inspect} (iprx_ + 32-byte lowercase hex)")
  end
end
