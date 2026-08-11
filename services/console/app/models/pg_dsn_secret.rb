# A Postgres upstream credential: a connection-string (DSN) resolved from a
# secret source, plus an optional SET ROLE for the upstream session. Delivered to
# iron-proxy in the single-listener `postgres` list, where it is keyed for routing
# by `database` (the dbname a client sends to reach this upstream). Multiple
# secrets may target the same database so different principals can route that
# database through different upstream roles. Principal sync emits only one
# effective route per database, chosen by grant priority.
#
# `foreign_id` is also required: it identifies the upstream for credential
# delivery (env-var supplied DSNs) and is the stable handle operators reference.
# The listener bind address and client auth remain proxy-host deployment concerns
# and are not modeled here.
class PgDsnSecret < ApplicationRecord
  oid_prefix "pgs"

  include ForeignIdCollisionGuard
  include SyncConfigCacheInvalidation

  URL_SAFE_FORMAT = /\A[A-Za-z0-9\-._~]+\z/
  URL_SAFE_MESSAGE = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"

  # A Postgres GUC name: a bare identifier, or a dotted class.name custom
  # variable. Mirrors the proxy's validation so the control plane rejects names
  # the proxy would refuse to pin.
  GUC_NAME_FORMAT = /\A[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)?\z/
  # role / session_authorization are managed via the role field and are always
  # blocked by the proxy's role policy; they may not appear as pinned settings.
  RESERVED_SETTING_NAMES = %w[role session_authorization].freeze

  # A setting's `value_from` reference takes exactly one of these keys.
  VALUE_FROM_KEYS = %w[principal_label principal_field proxy_label].freeze
  # Principal attributes a `principal_field` reference may name. `id` resolves
  # to the principal oid and `console_user_id` to the associated user oid; raw
  # database primary keys are never exposed as setting values.
  PRINCIPAL_FIELDS = %w[
    id namespace foreign_id name kind slack_user_id slack_channel_id slack_team_id slack_email
    console_user_id console_user_email slack_history_channel_ids
  ].freeze
  # Legacy `principal_label` selectors that canonicalize to a first-class
  # `principal_field` on save. The `email` label stays on the compatibility
  # shim: unlike these names, it is plausible as a custom label on principals
  # of other kinds, where a rewrite would change what it resolves to.
  IDENTITY_PRINCIPAL_LABEL_FIELDS = {
    "kind" => "kind",
    "slack_user_id" => "slack_user_id",
    "slack_channel_id" => "slack_channel_id",
    "slack_team_id" => "slack_team_id",
    "slack_email" => "slack_email",
    "console-user-id" => "console_user_id"
  }.freeze

  has_one :dsn_source, class_name: "SecretSource", dependent: :destroy
  has_many :grants, dependent: :destroy
  belongs_to :created_by, class_name: "User"

  before_validation :normalize_identity_principal_label_settings

  # One entry in the proxy's synced `postgres` list, keyed for routing by
  # `database`. The opaque id is carried too so the proxy can refer back to the
  # canonical resource (it ignores fields it does not use). The DSN reuses the
  # shared secrets source shape.
  def to_proxy_dsn(principal: nil, proxy: nil)
    entry = {
      "id" => oid,
      "foreign_id" => foreign_id,
      "database" => database,
      "dsn" => dsn_source&.to_proxy_source
    }
    entry["role"] = role if role.present?
    rendered_settings = proxy_settings(principal: principal, proxy: proxy)
    entry["settings"] = rendered_settings if rendered_settings.present?
    entry
  end

  # The pinned session settings as the proxy expects them: an ordered array of
  # { "name", "value" } objects. Normalizes whatever shape was stored (string
  # keys, blank rows) into the canonical form, dropping entries without a name,
  # and resolves `value_from` references against the given principal/proxy.
  def proxy_settings(principal: nil, proxy: nil)
    Array(settings).filter_map do |s|
      next unless s.is_a?(Hash)

      name = s.with_indifferent_access[:name].presence
      next if name.blank?
      { "name" => name, "value" => setting_value(s, principal, proxy) }
    end
  end

  def proxy_label_settings?
    Array(settings).any? do |setting|
      next false unless setting.is_a?(Hash)

      ref = setting.with_indifferent_access[:value_from]
      next false unless ref.is_a?(Hash)

      ref[:proxy_label].present?
    end
  end

  validates :namespace, presence: true, format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }
  validates :foreign_id, presence: true, uniqueness: true,
            format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }
  validates :database, presence: true
  validate :labels_is_a_hash
  validate :settings_are_valid
  validate :dsn_source_present
  validate :database_matches_inline_dsn

  private

  def normalize_identity_principal_label_settings
    return unless settings.is_a?(Array)

    normalized = settings.map do |setting|
      next setting unless setting.is_a?(Hash)

      indifferent_setting = setting.with_indifferent_access
      value_from = indifferent_setting[:value_from]
      next setting unless value_from.is_a?(Hash)

      field = IDENTITY_PRINCIPAL_LABEL_FIELDS[value_from[:principal_label].to_s]
      next setting unless field

      indifferent_setting.except(:value_from).merge(
        value_from: value_from.except(:principal_label).merge(principal_field: field)
      ).to_h
    end
    self.settings = normalized unless normalized == settings
  end

  def labels_is_a_hash
    errors.add(:labels, "must be a hash") unless labels.is_a?(Hash)
  end

  # The concrete value the proxy should pin: the stored literal, or the
  # principal/proxy attribute or label a `value_from` reference names. References
  # resolve to "" when no principal/proxy is given or the label is absent, so
  # RLS-style policies fail closed rather than seeing a literal placeholder.
  def setting_value(setting, principal, proxy)
    setting = setting.with_indifferent_access
    ref = setting[:value_from]
    return setting[:value].to_s unless ref.is_a?(Hash)

    label = ref[:principal_label]
    if label.present?
      if PrincipalIdentityLabels.promoted?(principal, label)
        return PrincipalIdentityLabels.value(principal, label).to_s
      end

      return principal&.labels&.fetch(label.to_s, "").to_s
    end

    proxy_label = ref[:proxy_label]
    return proxy&.labels&.fetch(proxy_label.to_s, "").to_s if proxy_label.present?

    return "" unless principal

    case ref[:principal_field].to_s
    when "id" then principal.oid
    when "namespace" then principal.namespace.to_s
    when "foreign_id" then principal.foreign_id.to_s
    when "name" then principal.name.to_s
    when "kind" then principal.kind.to_s
    when "slack_user_id" then principal.slack_user_id.to_s
    when "slack_channel_id" then principal.slack_channel_id.to_s
    when "slack_team_id" then principal.slack_team_id.to_s
    when "slack_email" then principal.slack_email.to_s
    when "console_user_id" then principal.console_user&.oid.to_s
    when "console_user_email" then principal.console_user_email.to_s
    when "slack_history_channel_ids" then JSON.generate(principal.slack_history_channel_ids)
    else "" # Invalid persisted or unsaved settings fail closed.
    end
  end

  # Settings must be an array of { name, value } or { name, value_from }
  # objects with valid, unique GUC names, mirroring the proxy's compileSettings
  # so an upstream the proxy would reject can't be saved here. Empty is fine
  # (the default).
  def settings_are_valid
    return errors.add(:settings, "must be an array") unless settings.is_a?(Array)

    seen = Set.new
    settings.each_with_index do |setting, i|
      reason = setting_error(setting, seen)
      errors.add(:settings, "[#{i}] #{reason}") if reason
    end
  end

  # Why setting is invalid, or nil when it's well-formed. Records the lowercased
  # name in seen so a later occurrence is reported as a duplicate.
  def setting_error(setting, seen)
    return "must be an object" unless setting.is_a?(Hash)

    setting = setting.with_indifferent_access
    name = setting[:name].to_s
    return "name is required" if name.blank?
    return "invalid setting name #{name.inspect}" unless name.match?(GUC_NAME_FORMAT)

    lower = name.downcase
    return "#{name.inspect} is managed by the proxy; use the role field" if RESERVED_SETTING_NAMES.include?(lower)
    return "duplicate setting #{name.inspect}" unless seen.add?(lower)

    value_from_error(setting)
  end

  # Why setting's `value_from` reference is invalid, or nil when it's absent or
  # well-formed. Rejecting bad references at save time is the point of the
  # structured shape: a typo'd field is an error here, not an empty string the
  # proxy quietly pins at sync time.
  def value_from_error(setting)
    setting = setting.with_indifferent_access
    ref = setting[:value_from]
    return nil if ref.nil?
    return "value and value_from are mutually exclusive" unless setting[:value].nil?
    return "value_from must be an object" unless ref.is_a?(Hash)

    keys = ref.keys.map(&:to_s)
    unless keys.length == 1 && VALUE_FROM_KEYS.include?(keys.first)
      return "value_from must have exactly one of #{VALUE_FROM_KEYS.join(" or ")}"
    end

    label = ref[:principal_label]
    field = ref[:principal_field]
    return "principal_label can't be blank" if keys.first == "principal_label" && label.to_s.blank?
    proxy_label = ref[:proxy_label]
    return "proxy_label can't be blank" if keys.first == "proxy_label" && proxy_label.to_s.blank?
    if keys.first == "principal_field" && !PRINCIPAL_FIELDS.include?(field.to_s)
      return "unknown principal_field #{field.to_s.inspect} (one of: #{PRINCIPAL_FIELDS.join(", ")})"
    end

    nil
  end

  def dsn_source_present
    errors.add(:dsn_source, "can't be blank") if dsn_source.blank?
  end

  # Enforce the spec invariant database == the DSN's database, but only where the
  # DSN is inspectable: a control_plane (inline) source. Other source types
  # resolve their value on the proxy host, so the proxy is the authority there.
  def database_matches_inline_dsn
    return if database.blank? # presence handles the empty case
    src = dsn_source
    return unless src&.source_type == "control_plane"
    dsn = inline_dsn_value(src)
    return if dsn.blank?

    begin
      parsed = PG::Connection.conninfo_parse(dsn)
    rescue PG::Error
      return # malformed inline DSN: let the proxy be the authority
    end

    named = parsed.find { |o| o[:keyword] == "dbname" }&.dig(:val)
    if named.blank?
      errors.add(:database, "DSN names no database; it must match #{database.inspect}")
    elsif named != database
      errors.add(:database, "must match the DSN database (#{named.inspect})")
    end
  end

  # The literal DSN string a control_plane source resolves to, honoring json_key
  # the way the proxy would (parse JSON, then extract the key). Returns nil when
  # the value can't be inspected.
  def inline_dsn_value(src)
    key = src.config.is_a?(Hash) ? src.config["json_key"] : nil
    return src.secret if key.blank?
    JSON.parse(src.secret.to_s)[key]
  rescue JSON::ParserError, TypeError
    nil
  end
end
