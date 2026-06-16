require "ipaddr"

class RequestRule < ApplicationRecord
  oid_prefix "rqr"

  include SyncConfigOwnerInvalidation

  HTTP_METHODS = %w[GET HEAD POST PUT PATCH DELETE OPTIONS CONNECT].freeze
  METHOD_WILDCARD = "*".freeze

  # A rule hangs off exactly one credential type.
  belongs_to :static_secret, optional: true
  belongs_to :gcp_auth_secret, optional: true
  belongs_to :aws_auth_secret, optional: true
  belongs_to :oauth_token_secret, optional: true
  belongs_to :hmac_secret, optional: true

  OWNER_ASSOCIATIONS = %i[static_secret gcp_auth_secret aws_auth_secret oauth_token_secret hmac_secret].freeze

  default_scope { order(:position) }

  # Maps to the iron-proxy `hostmatch.RuleConfig` shape. Note the proxy uses
  # `methods` where centaur-console stores `http_methods`. Blank fields are omitted
  # so they decode as the proxy's omitempty defaults.
  def to_proxy_rule
    rule = {}
    rule["host"] = host if host.present?
    rule["cidr"] = cidr if cidr.present?
    rule["methods"] = http_methods if http_methods.present?
    rule["paths"] = paths if paths.present?
    rule
  end

  validates :position, presence: true, numericality: { only_integer: true }
  validate :host_xor_cidr
  validate :cidr_is_valid
  validate :http_methods_are_valid
  validate :paths_are_valid
  validate :at_most_one_owner

  private

  def at_most_one_owner
    # Check the association object, not just the FK column: when built through a
    # parent (parent.rules.build / parent.rules =) autosave validates this record
    # before the parent is persisted, so the FK is still nil but the inverse
    # association is already set.
    set = OWNER_ASSOCIATIONS.count { |assoc| send(assoc).present? }
    return if set <= 1
    errors.add(:base, "must belong to at most one of #{OWNER_ASSOCIATIONS.join(", ")}")
  end

  def host_xor_cidr
    if host.present? && cidr.present?
      errors.add(:base, "host and cidr are mutually exclusive")
    elsif host.blank? && cidr.blank?
      errors.add(:base, "either host or cidr must be present")
    end
  end

  def cidr_is_valid
    return if cidr.blank?
    IPAddr.new(cidr)
  rescue IPAddr::Error
    errors.add(:cidr, "is not a valid CIDR")
  end

  def http_methods_are_valid
    unless http_methods.is_a?(Array)
      errors.add(:http_methods, "must be an array")
      return
    end
    http_methods.each do |m|
      next if m == METHOD_WILDCARD || HTTP_METHODS.include?(m)
      errors.add(:http_methods, "#{m.inspect} is not a supported HTTP method")
    end
  end

  def paths_are_valid
    unless paths.is_a?(Array)
      errors.add(:paths, "must be an array")
      return
    end
    paths.each do |p|
      unless p.is_a?(String) && p.start_with?("/")
        errors.add(:paths, "#{p.inspect} must be a string starting with /")
      end
    end
  end
end
