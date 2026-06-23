class GcpIdTokenSecret < ApplicationRecord
  oid_prefix "gid"

  include ForeignIdCollisionGuard
  include SyncConfigCacheInvalidation

  URL_SAFE_FORMAT = /\A[A-Za-z0-9\-._~]+\z/
  URL_SAFE_MESSAGE = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"

  HEADERS = %w[authorization x-serverless-authorization].freeze
  DEFAULT_HEADER = "authorization".freeze

  has_one :keyfile_source, class_name: "SecretSource", dependent: :destroy
  has_many :rules, class_name: "RequestRule", dependent: :destroy
  has_many :grants, dependent: :destroy
  belongs_to :created_by, class_name: "User"

  before_validation :normalize_header

  # Maps to one entry in the iron-proxy `transforms` array as a gcp_id_token
  # transform. The proxy defaults to Authorization when header is omitted.
  def to_proxy_transform
    config = {
      "keyfile" => keyfile_source.to_proxy_source,
      "audience" => audience,
      "rules" => rules.map(&:to_proxy_rule)
    }
    config["header"] = header if header.present?
    { "name" => "gcp_id_token", "config" => config }
  end

  def proxy_conflict_targets
    [ "header:#{header.presence || DEFAULT_HEADER}" ]
  end

  validates :namespace, presence: true, format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }
  validates :foreign_id, uniqueness: { scope: :namespace, allow_nil: true },
            format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }, allow_nil: true
  validates :audience, presence: true
  validate :labels_is_a_hash
  validate :keyfile_source_present
  validate :header_is_supported
  validate :at_least_one_rule

  private

  def normalize_header
    self.header = header.to_s.strip.downcase.presence
  end

  def labels_is_a_hash
    errors.add(:labels, "must be a hash") unless labels.is_a?(Hash)
  end

  def keyfile_source_present
    errors.add(:keyfile_source, "can't be blank") unless keyfile_source
  end

  def header_is_supported
    return if header.blank? || HEADERS.include?(header)
    errors.add(:header, "must be one of #{HEADERS.join(", ")}")
  end

  def at_least_one_rule
    errors.add(:rules, "must include at least one rule") if rules.empty?
  end
end
