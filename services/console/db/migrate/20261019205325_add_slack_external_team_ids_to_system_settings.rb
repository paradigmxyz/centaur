class AddSlackExternalTeamIdsToSystemSettings < ActiveRecord::Migration[8.1]
  def up
    add_column :system_settings, :slack_external_team_ids, :text, array: true, default: [], null: false

    # Carry over the allowlist deployments previously set through the Helm
    # value so upgrading does not start ignoring admitted Slack Connect orgs.
    team_ids = ENV.fetch("SLACKBOT_EXTERNAL_ORG_ALLOWLIST", "").split(/[\s,]+/).compact_blank.map(&:upcase).uniq
    return if team_ids.empty?

    literal = connection.quote(PG::TextEncoder::Array.new.encode(team_ids))
    execute <<~SQL
      INSERT INTO system_settings (singleton, slack_external_team_ids, created_at, updated_at)
      VALUES (true, #{literal}, now(), now())
      ON CONFLICT (singleton) DO UPDATE SET slack_external_team_ids = EXCLUDED.slack_external_team_ids
    SQL
  end

  def down
    remove_column :system_settings, :slack_external_team_ids
  end
end
