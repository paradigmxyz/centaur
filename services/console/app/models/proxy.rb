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
  # Optional second principal: the human whose turn is currently running.
  # When set, the rendered config unions in their always-available direct
  # credential grants (see PrincipalSyncConfigSnapshot).
  belongs_to :requester_principal, class_name: "Principal", optional: true

  validates :name, presence: true
  validates :bearer_token_hash, presence: true, uniqueness: true
  validate :labels_are_string_map
  validate :token_matches_format, on: :create

  before_validation :normalize_labels
  before_validation :issue_token, on: :create
  before_save :stamp_principal_assignment, if: :will_save_change_to_principal_id?
  before_save :stamp_requester_principal_assignment, if: :will_save_change_to_requester_principal_id?

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
    PrincipalSyncConfigSnapshot.sync_config_for_proxy(self, sandbox_entitlements_hosts: sandbox_entitlements_hosts)
  end

  # Opaque, deterministic fingerprint of the exact config delivered by the
  # proxy sync endpoint.
  def config_hash
    sync_config_snapshot.fetch(:config_hash)
  end

  def self.sandbox_entitlements_hosts
    [ Principal.host_from_url(ENV["CENTAUR_CONSOLE_URL"]) ]
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

  # Stamp (or clear) the assignment time whenever principal_id changes, so the
  # column always reflects the current assignment.
  def stamp_principal_assignment
    self.principal_assigned_at = principal_id ? Time.current : nil
  end

  def stamp_requester_principal_assignment
    self.requester_principal_assigned_at = requester_principal_id ? Time.current : nil
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
