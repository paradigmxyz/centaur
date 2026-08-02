class PrincipalIdentifierCompatibility
  IDENTITY_LABEL_FIELDS = %w[
    console-user-id
    slack_user_id
    slack_channel_id
    slack_team_id
    slack_email
    discord_guild_id
    discord_channel_id
    linear_issue_id
    teams_user_id
    teams_conversation_id
    workflow_name
    google_subject
    google_email
  ].freeze
  RESERVED_LABEL_FIELDS = ([ "kind" ] + IDENTITY_LABEL_FIELDS).freeze

  MAPPINGS = [
    { scheme: "console_user", subject: "console-user-id" },
    {
      scheme: "slack_user",
      subject: "slack_user_id",
      issuer: "slack_team_id",
      metadata: "slack_email"
    },
    { scheme: "slack_channel", subject: "slack_channel_id", issuer: "slack_team_id" },
    { scheme: "linear_issue", subject: "linear_issue_id" },
    { scheme: "teams_user", subject: "teams_user_id" },
    { scheme: "teams_conversation", subject: "teams_conversation_id" },
    { scheme: "workflow", subject: "workflow_name" },
    { scheme: "google_user", subject: "google_subject", metadata: "google_email" }
  ].freeze

  class << self
    def replace!(principal, identifier_attributes)
      existing = active_identifiers(principal)
      retained = []
      Array(identifier_attributes).each do |attributes|
        values = attributes.respond_to?(:to_h) ? attributes.to_h : {}
        scheme = normalize(values["scheme"] || values[:scheme])
        issuer = normalize(values["issuer"] || values[:issuer]).to_s
        subject = normalize(values["subject"] || values[:subject])
        metadata = values["metadata"] || values[:metadata] || {}
        identifier = existing.find do |record|
          record.scheme == scheme && record.issuer == issuer && record.subject == subject && !retained.include?(record)
        end
        identifier ||= principal.principal_identifiers.build(
          scheme: scheme,
          issuer: issuer,
          subject: subject
        )
        identifier.metadata = metadata
        retained << identifier
      end
      existing.each { |identifier| identifier.mark_for_destruction unless retained.include?(identifier) }
      principal.identifiers_assigned_explicitly = true
    end

    def promote!(principal)
      labels = principal.labels.to_h
      if labels.key?("kind") && !principal.will_save_change_to_kind?
        principal.kind = labels["kind"]
      elsif !principal.will_save_change_to_kind? && principal.kind == Principal::UNKNOWN_KIND
        principal.kind = infer_kind(principal, labels) || principal.kind
      end
      return if principal.identifiers_assigned_explicitly?

      MAPPINGS.each { |mapping| promote_mapping!(principal, labels, mapping) }
      promote_discord!(principal, labels)
    end

    def mirror!(principal)
      aliases = aliases_for(principal, include_unknown_kind: false)
      principal[:labels] = principal.labels.to_h.except(*RESERVED_LABEL_FIELDS).merge(aliases)
    end

    def response_labels(principal)
      principal.labels.to_h
        .except(*RESERVED_LABEL_FIELDS)
        .merge(aliases_for(principal, include_unknown_kind: true))
    end

    def aliases_for(principal, include_unknown_kind:)
      aliases = {}
      if principal.kind.present? && (include_unknown_kind || principal.kind != Principal::UNKNOWN_KIND)
        aliases["kind"] = principal.kind
      end

      records = active_identifiers(principal)
      project_simple_alias!(aliases, records, "console_user", "console-user-id")
      project_simple_alias!(aliases, records, "linear_issue", "linear_issue_id")
      project_simple_alias!(aliases, records, "teams_user", "teams_user_id")
      project_simple_alias!(aliases, records, "teams_conversation", "teams_conversation_id")
      project_simple_alias!(aliases, records, "workflow", "workflow_name")
      project_simple_alias!(aliases, records, "google_user", "google_subject", metadata: "google_email")
      project_discord_aliases!(aliases, records)
      project_slack_aliases!(aliases, records)
      aliases
    end

    private

    def infer_kind(principal, labels)
      foreign_id = principal.foreign_id.to_s
      return "console_user" if present?(labels["console-user-id"]) || foreign_id.start_with?("console-user-")
      if %w[warm-pool-bootstrap workflow-host].include?(labels["purpose"]) ||
          %w[warm-pool-bootstrap workflow-host].include?(foreign_id)
        return "service"
      end
      return "workflow" if present?(labels["workflow_name"]) || foreign_id.start_with?("workflow-")
      return "discord_channel" if present?(labels["discord_guild_id"]) || foreign_id.start_with?("discord-channel-")
      return "linear_issue" if present?(labels["linear_issue_id"]) || foreign_id.start_with?("linear-issue-")
      return "teams_user" if present?(labels["teams_user_id"]) || foreign_id.start_with?("teams-user-")
      if present?(labels["teams_conversation_id"]) || foreign_id.start_with?("teams-conversation-")
        return "teams_conversation"
      end

      slack_channel_id = normalize(labels["slack_channel_id"])
      return "slack_dm" if slack_channel_id&.upcase&.start_with?("D")
      return "slack_channel" if slack_channel_id
      "slack_dm" if present?(labels["slack_user_id"]) || foreign_id.start_with?("slack-user-")
    end

    def present?(value)
      normalize(value).present?
    end

    def promote_mapping!(principal, labels, mapping)
      relevant_keys = mapping.values_at(:subject, :issuer, :metadata).compact
      return if relevant_keys.none? { |key| labels.key?(key) }

      records = active_identifiers(principal).select { |record| record.scheme == mapping[:scheme] }
      subject = if labels.key?(mapping[:subject])
        normalize(labels[mapping[:subject]])
      elsif records.one?
        records.first.subject
      end
      return if subject.nil? && !labels.key?(mapping[:subject])

      if subject.blank?
        records.each(&:mark_for_destruction)
        return
      end

      issuer = if mapping[:issuer] && labels.key?(mapping[:issuer])
        normalize(labels[mapping[:issuer]]).to_s
      elsif records.one?
        records.first.issuer
      else
        ""
      end
      metadata = records.one? ? records.first.metadata.to_h : {}
      if mapping[:metadata] && labels.key?(mapping[:metadata])
        email = normalize(labels[mapping[:metadata]])
        metadata = email ? metadata.merge("email" => email) : metadata.except("email")
      end

      replace_scheme!(principal, mapping[:scheme], issuer:, subject:, metadata:)
    end

    def promote_discord!(principal, labels)
      return unless labels.key?("discord_guild_id") || labels.key?("discord_channel_id")

      guild_id = normalize(labels["discord_guild_id"])
      channel_id = normalize(labels["discord_channel_id"])
      replace_scheme!(principal, "discord_channel", issuer: guild_id.to_s, subject: channel_id) if channel_id
      replace_scheme!(principal, "discord_guild", issuer: "", subject: guild_id) if guild_id && !channel_id
      clear_scheme!(principal, "discord_channel") unless channel_id
      clear_scheme!(principal, "discord_guild") unless guild_id && !channel_id
    end

    def replace_scheme!(principal, scheme, issuer:, subject:, metadata: {})
      records = active_identifiers(principal).select { |record| record.scheme == scheme }
      matching = records.find { |record| record.issuer == issuer && record.subject == subject }
      records.each { |record| record.mark_for_destruction unless record == matching }
      if matching
        matching.metadata = metadata
      else
        principal.principal_identifiers.build(
          namespace: principal.namespace,
          scheme: scheme,
          issuer: issuer,
          subject: subject,
          metadata: metadata
        )
      end
    end

    def clear_scheme!(principal, scheme)
      active_identifiers(principal)
        .select { |record| record.scheme == scheme }
        .each(&:mark_for_destruction)
    end

    def project_simple_alias!(aliases, records, scheme, subject_label, metadata: nil)
      record = single_identifier(records, scheme)
      return unless record

      aliases[subject_label] = record.subject
      aliases[metadata] = record.metadata["email"] if metadata && record.metadata["email"].present?
    end

    def project_discord_aliases!(aliases, records)
      channels = records.select { |record| record.scheme == "discord_channel" }
      guilds = records.select { |record| record.scheme == "discord_guild" }
      if channels.one? && guilds.empty?
        aliases["discord_channel_id"] = channels.first.subject
        aliases["discord_guild_id"] = channels.first.issuer if channels.first.issuer.present?
      elsif guilds.one? && channels.empty?
        aliases["discord_guild_id"] = guilds.first.subject
      end
    end

    def project_slack_aliases!(aliases, records)
      user = single_identifier(records, "slack_user")
      channel = single_identifier(records, "slack_channel")
      aliases["slack_user_id"] = user.subject if user
      aliases["slack_channel_id"] = channel.subject if channel
      aliases["slack_email"] = user.metadata["email"] if user&.metadata&.[]("email").present?

      slack_identifiers = records.select { |record| %w[slack_user slack_channel].include?(record.scheme) }
      issuers = slack_identifiers.map(&:issuer).uniq
      aliases["slack_team_id"] = issuers.first if issuers.one? && issuers.first.present?
    end

    def single_identifier(records, scheme)
      matches = records.select { |record| record.scheme == scheme }
      matches.one? ? matches.first : nil
    end

    def active_identifiers(principal)
      principal.principal_identifiers.to_a.reject(&:marked_for_destruction?)
    end

    def normalize(value)
      value.to_s.strip.presence
    end
  end
end
