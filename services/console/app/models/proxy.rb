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
  validate :labels_are_string_map
  validate :token_matches_format, on: :create

  before_validation :normalize_labels
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

  def sync_config_snapshot(sandbox_entitlements_hosts: self.class.sandbox_entitlements_hosts)
    config = rendered_principal_config
    config = with_sandbox_entitlements_secret(config, sandbox_entitlements_hosts: sandbox_entitlements_hosts)
    { config_hash: config_hash_for(config), config: config }
  end

  # Opaque, deterministic fingerprint of the exact config delivered by the
  # proxy sync endpoint.
  def config_hash
    sync_config_snapshot.fetch(:config_hash)
  end

  def self.sandbox_entitlements_hosts
    [ Principal.host_from_url(ENV["CENTAUR_CONSOLE_URL"]) ]
  end

  def config_hash_for(config)
    payload = config.merge(
      "principal" => principal&.oid,
      "principal_assigned_at" => principal_assigned_at&.utc&.iso8601,
      "proxy_labels" => labels || {}
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

  def normalize_labels
    self.labels = {} if labels.nil?
  end

  def labels_are_string_map
    return errors.add(:labels, "must be a hash") unless labels.is_a?(Hash)

    labels.each do |key, value|
      errors.add(:labels, "keys must be strings") unless key.is_a?(String)
      errors.add(:labels, "values must be strings") unless value.is_a?(String)
    end
  end

  def rendered_principal_config
    return Principal::EMPTY_CONFIG.deep_dup unless principal

    snapshot = PrincipalSyncConfigSnapshot.fetch_for(principal)
    copy = snapshot.config.deep_dup
    templates = snapshot.postgres_setting_templates
    copy["postgres"] = proxy_specific_postgres(copy["postgres"], templates) if templates.any?
    copy
  end

  def proxy_specific_postgres(postgres, templates)
    Array(postgres).map do |entry|
      next entry unless entry.is_a?(Hash)

      template = templates[entry["id"].to_s]
      next entry unless template

      rendered_settings = proxy_specific_postgres_settings(entry["settings"], template)
      entry.merge("settings" => rendered_settings)
    end
  end

  def proxy_specific_postgres_settings(rendered_settings, template_settings)
    rendered_by_name = Array(rendered_settings).each_with_object({}) do |setting, values|
      next unless setting.is_a?(Hash)

      name = setting["name"].presence || setting[:name].presence
      values[name] = setting["value"] || setting[:value] if name.present?
    end

    Array(template_settings).filter_map do |setting|
      next unless setting.is_a?(Hash)

      name = setting["name"].presence || setting[:name].presence
      next if name.blank?

      value = proxy_label_setting_value(setting)
      value = rendered_by_name.fetch(name, "") if value.nil?
      { "name" => name, "value" => value }
    end
  end

  def proxy_label_setting_value(setting)
    ref = setting["value_from"] || setting[:value_from]
    return nil unless ref.is_a?(Hash)

    proxy_label = ref["proxy_label"] || ref[:proxy_label]
    return nil if proxy_label.blank?

    labels&.fetch(proxy_label.to_s, "").to_s
  end

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
