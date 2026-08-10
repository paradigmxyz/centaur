class MigratePgDsnIdentityLabelsToPrincipalFields < ActiveRecord::Migration[8.1]
  IDENTITY_FIELDS = %w[
    kind slack_user_id slack_channel_id slack_team_id slack_email
  ].freeze

  class MigrationPgDsnSecret < ActiveRecord::Base
    self.table_name = "pg_dsn_secrets"
  end

  def up
    rewrite_settings(from: "principal_label", to: "principal_field")
  end

  def down
    rewrite_settings(from: "principal_field", to: "principal_label")
  end

  private

  def rewrite_settings(from:, to:)
    MigrationPgDsnSecret.find_each do |secret|
      rewritten = rewrite_secret_settings(secret.settings, from:, to:)
      next if rewritten == secret.settings

      secret.update_columns(settings: rewritten, updated_at: Time.current)
    end
  end

  def rewrite_secret_settings(settings, from:, to:)
    Array(settings).map do |setting|
      next setting unless setting.is_a?(Hash)

      value_from = setting["value_from"]
      next setting unless value_from.is_a?(Hash)

      field = value_from[from]
      next setting unless IDENTITY_FIELDS.include?(field)

      setting.merge("value_from" => value_from.except(from).merge(to => field))
    end
  end
end
