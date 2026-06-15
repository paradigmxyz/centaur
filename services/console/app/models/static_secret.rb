class StaticSecret < ApplicationRecord
  oid_prefix "ssr"

  include ForeignIdCollisionGuard

  has_many :grants, dependent: :destroy

  URL_SAFE_FORMAT = /\A[A-Za-z0-9\-._~]+\z/
  URL_SAFE_MESSAGE = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"

  INJECT_CONFIG_SCHEMA = JSONSchemer.schema({
    "type" => "object",
    "additionalProperties" => false,
    "properties" => {
      "header" => { "type" => "string", "minLength" => 1 },
      "formatter" => { "type" => "string" },
      "query_param" => { "type" => "string", "minLength" => 1 }
    },
    "oneOf" => [
      { "required" => [ "header" ] },
      { "required" => [ "query_param" ] }
    ]
  })

  REPLACE_CONFIG_SCHEMA = JSONSchemer.schema({
    "type" => "object",
    "additionalProperties" => false,
    "required" => [ "proxy_value" ],
    "properties" => {
      "proxy_value" => { "type" => "string", "minLength" => 1 },
      "match_headers" => { "type" => "array", "items" => { "type" => "string" } },
      "match_body" => { "type" => "boolean" },
      "match_path" => { "type" => "boolean" },
      "match_query" => { "type" => "boolean" },
      "require" => { "type" => "boolean" }
    }
  })

  has_one :source, class_name: "SecretSource", dependent: :destroy
  has_many :rules, class_name: "RequestRule", dependent: :destroy
  belongs_to :created_by, class_name: "User"

  # Maps to a single entry in the iron-proxy `secrets` transform array. The
  # caller is responsible for skipping secrets without a source.
  def to_proxy_secret
    entry = {
      "source" => source&.to_proxy_source,
      "rules" => rules.map(&:to_proxy_rule)
    }
    entry["inject"] = inject_config if inject_config.present?
    entry["replace"] = replace_config if replace_config.present?
    entry
  end

  validates :namespace, presence: true, format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }
  validates :foreign_id, uniqueness: { scope: :namespace, allow_nil: true },
            format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }, allow_nil: true
  validate :labels_is_a_hash
  validate :exactly_one_of_inject_or_replace
  validate :inject_config_matches_schema
  validate :replace_config_matches_schema

  private

  def labels_is_a_hash
    errors.add(:labels, "must be a hash") unless labels.is_a?(Hash)
  end

  def exactly_one_of_inject_or_replace
    present = [ inject_config.present?, replace_config.present? ].count(true)
    if present.zero?
      errors.add(:base, "must define one of inject_config or replace_config")
    elsif present > 1
      errors.add(:base, "inject_config and replace_config are mutually exclusive")
    end
  end

  def inject_config_matches_schema
    validate_against_schema(:inject_config, inject_config, INJECT_CONFIG_SCHEMA)
  end

  def replace_config_matches_schema
    validate_against_schema(:replace_config, replace_config, REPLACE_CONFIG_SCHEMA)
  end

  def validate_against_schema(attr, value, schema)
    return if value.blank?
    unless value.is_a?(Hash)
      errors.add(attr, "must be a hash")
      return
    end
    schema.validate(value).each do |err|
      pointer = err["data_pointer"].presence || "(root)"
      errors.add(attr, "#{pointer} #{err["error"]}")
    end
  end
end
