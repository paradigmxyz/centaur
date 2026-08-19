class MigratePgDsnIdentityLabelsToPrincipalFields < ActiveRecord::Migration[8.1]
  # Legacy identity label => first-class principal field. The `email` label is
  # deliberately not migrated: it is plausible as a custom label on principals
  # of other kinds, where a rewrite would change what it resolves to.
  LABEL_FIELDS = {
    "kind" => "kind",
    "slack_user_id" => "slack_user_id",
    "slack_channel_id" => "slack_channel_id",
    "slack_team_id" => "slack_team_id",
    "slack_email" => "slack_email",
    "console-user-id" => "console_user_id"
  }.freeze

  class MigrationPgDsnSecret < ActiveRecord::Base
    self.table_name = "pg_dsn_secrets"
  end

  def up
    rewrite_settings(from: "principal_label", to: "principal_field", mapping: LABEL_FIELDS)
  end

  def down
    rewrite_settings(from: "principal_field", to: "principal_label", mapping: LABEL_FIELDS.invert)
  end

  private

  def rewrite_settings(from:, to:, mapping:)
    MigrationPgDsnSecret.find_each do |secret|
      rewritten = rewrite_secret_settings(secret.settings, from:, to:, mapping:)
      next if rewritten == secret.settings

      secret.update_columns(settings: rewritten, updated_at: Time.current)
    end
  end

  def rewrite_secret_settings(settings, from:, to:, mapping:)
    Array(settings).map { |setting| rewrite_setting(setting, from:, to:, mapping:) }
  end

  def rewrite_setting(setting, from:, to:, mapping:)
    return setting unless setting.is_a?(Hash) && setting["value_from"].is_a?(Hash)

    value_from = setting.fetch("value_from")
    mapped = mapping[value_from[from]]
    return setting unless mapped

    setting.merge("value_from" => value_from.except(from).merge(to => mapped))
  end
end
