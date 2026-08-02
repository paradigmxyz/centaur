class PrincipalIdentifierFilter
  class Invalid < StandardError; end

  SIMPLE_LEGACY_FILTERS = {
    "console-user-id" => "console_user",
    "linear_issue_id" => "linear_issue",
    "teams_user_id" => "teams_user",
    "teams_conversation_id" => "teams_conversation",
    "workflow_name" => "workflow",
    "google_subject" => "google_user"
  }.freeze
  NATIVE_FILTER_FIELDS = %i[identifier_scheme identifier_issuer identifier_subject].freeze

  def initialize(params:, labels:)
    @params = params
    @labels = labels
  end

  def apply(scope)
    scope = apply_kind_filter(scope)
    legacy = labels.extract!(*PrincipalIdentifierCompatibility::IDENTITY_LABEL_FIELDS)
    legacy.each_key { |field| Rails.logger.warn("deprecated_principal_label_filter field=#{field}") }
    if native_identifier_filter? && legacy.any?
      raise Invalid, "use either native identifier filters or legacy principal identity label filters"
    end

    return scope.where(id: native_identifier_scope.select(:principal_id)) if native_identifier_filter?

    apply_legacy_identifier_filters(scope, legacy)
  end

  private

  attr_reader :params, :labels

  def apply_kind_filter(scope)
    legacy_kind = labels.delete("kind")
    native_kind = params[:kind] if params.key?(:kind)
    if legacy_kind.present?
      Rails.logger.warn("deprecated_principal_label_filter field=kind")
      if native_kind.present? && native_kind.to_s != legacy_kind.to_s
        raise Invalid, "conflicting kind and labels[kind] filters"
      end
    end
    kind = native_kind.presence || legacy_kind
    kind.present? ? scope.where(kind: kind.to_s) : scope
  end

  def native_identifier_filter?
    NATIVE_FILTER_FIELDS.any? { |key| params.key?(key) }
  end

  def native_identifier_scope
    scheme = params[:identifier_scheme].to_s.strip
    raise Invalid, "identifier_scheme is required" if scheme.blank?

    identifiers = PrincipalIdentifier.where(scheme: scheme)
    identifiers = identifiers.where(issuer: params[:identifier_issuer].to_s) if params.key?(:identifier_issuer)
    identifiers = identifiers.where(subject: params[:identifier_subject].to_s) if params.key?(:identifier_subject)
    identifiers
  end

  def apply_legacy_identifier_filters(scope, legacy)
    scope = apply_legacy_slack_filters(scope, legacy)
    scope = apply_legacy_discord_filters(scope, legacy)
    SIMPLE_LEGACY_FILTERS.each do |label, scheme|
      value = legacy.delete(label)
      scope = scope.where(id: identifier_scope(scheme, subject: value).select(:principal_id)) if value.present?
    end
    google_email = legacy.delete("google_email")
    if google_email.present?
      scope = scope.where(id: identifier_scope("google_user", email: google_email).select(:principal_id))
    end
    scope
  end

  def apply_legacy_slack_filters(scope, legacy)
    team = legacy.delete("slack_team_id")
    user = legacy.delete("slack_user_id")
    email = legacy.delete("slack_email")
    channel = legacy.delete("slack_channel_id")

    if user.present? || email.present?
      scope = scope.where(id: identifier_scope(
        "slack_user", issuer: team, subject: user, email: email
      ).select(:principal_id))
    end
    if channel.present?
      scope = scope.where(id: identifier_scope(
        "slack_channel", issuer: team, subject: channel
      ).select(:principal_id))
    end
    if team.present? && user.blank? && email.blank? && channel.blank?
      scope = scope.where(id: PrincipalIdentifier.where(
        scheme: %w[slack_user slack_channel], issuer: team
      ).select(:principal_id))
    end
    scope
  end

  def apply_legacy_discord_filters(scope, legacy)
    guild = legacy.delete("discord_guild_id")
    channel = legacy.delete("discord_channel_id")
    if channel.present?
      return scope.where(id: identifier_scope(
        "discord_channel", issuer: guild, subject: channel
      ).select(:principal_id))
    end
    return scope unless guild.present?

    discord_ids = PrincipalIdentifier
      .where(scheme: "discord_channel", issuer: guild)
      .or(PrincipalIdentifier.where(scheme: "discord_guild", subject: guild))
    scope.where(id: discord_ids.select(:principal_id))
  end

  def identifier_scope(scheme, issuer: nil, subject: nil, email: nil)
    identifiers = PrincipalIdentifier.where(scheme: scheme)
    identifiers = identifiers.where(issuer: issuer) if issuer.present?
    identifiers = identifiers.where(subject: subject) if subject.present?
    identifiers = identifiers.where("metadata @> ?", { email: email }.to_json) if email.present?
    identifiers
  end
end
