require "test_helper"
require "securerandom"
require Rails.root.join("db/migrate/20260709223000_backfill_slack_channel_permissions_from_labels").to_s
require Rails.root.join("db/migrate/20260709224500_backfill_slack_dm_channel_permissions_from_user_labels").to_s

class SlackChannelPermissionTest < ActiveSupport::TestCase
  test "normalizes channel id and requires at least one permission" do
    permission = SlackChannelPermission.new(
      principal: principals(:acme_channel),
      channel_id: " c0123456789 ",
      channel_name: " general ",
      upload_enabled: true,
      download_enabled: false,
      history_enabled: false
    )

    assert_predicate permission, :valid?
    permission.save!
    assert_equal "C0123456789", permission.channel_id
    assert_equal "general", permission.channel_name

    empty = SlackChannelPermission.new(
      principal: principals(:acme_channel),
      channel_id: "C9999999999"
    )
    assert_not empty.valid?
    assert_includes empty.errors[:base], "Select at least one Slack permission"
  end

  test "rejects manually assigned slack DM permissions" do
    permission = SlackChannelPermission.new(
      principal: principals(:acme_channel),
      channel_id: "D0123456789",
      channel_name: "U0123456789",
      upload_enabled: true,
      download_enabled: true,
      history_enabled: true
    )

    assert_not permission.valid?
    assert_includes permission.errors[:channel_id], "must be the 1:1 DM for this principal's slack_user_id"
  end

  test "replace_for_principal replaces permission rows" do
    principal = principals(:acme_channel)

    SlackChannelPermission.replace_for_principal!(
      principal,
      [
        {
          channel_id: "c0123456789",
          channel_name: "general",
          upload_enabled: true,
          download_enabled: true,
          history_enabled: false
        }
      ]
    )

    permission = principal.slack_channel_permissions.reload.sole
    assert_equal "C0123456789", permission.channel_id
    assert_equal "general", permission.channel_name
    assert_equal true, permission.upload_enabled
    assert_equal true, permission.download_enabled
    assert_equal false, permission.history_enabled
  end

  test "protected slack DM permission cannot be destroyed" do
    principal = principals(:acme_user_bob)
    principal.update!(labels: { Principal::SLACK_USER_ID_LABEL => "U0123456789" })
    permission = SlackChannelPermission.create!(
      principal: principal,
      channel_id: "D0123456789",
      channel_name: "U0123456789",
      upload_enabled: true,
      download_enabled: true,
      history_enabled: true
    )

    assert_not permission.destroy
    assert_includes permission.errors[:base], "Slack DM permissions created from slack_user_id labels cannot be removed"
    assert SlackChannelPermission.exists?(permission.id)
  end

  test "replace_for_principal rejects removing protected slack DM permissions" do
    principal = principals(:acme_user_bob)
    principal.update!(labels: { Principal::SLACK_USER_ID_LABEL => "U0123456789" })
    protected_permission = SlackChannelPermission.create!(
      principal: principal,
      channel_id: "D0123456789",
      channel_name: "U0123456789",
      upload_enabled: true,
      download_enabled: true,
      history_enabled: true
    )
    SlackChannelPermission.create!(
      principal: principal,
      channel_id: "C0123456789",
      upload_enabled: true
    )

    assert_raises(ActiveRecord::RecordNotDestroyed) do
      SlackChannelPermission.replace_for_principal!(principal, [])
    end

    assert_includes principal.slack_channel_permissions.reload.map(&:id), protected_permission.id
  end

  test "label backfill migration creates all slack permissions" do
    principal = insert_principal_with_slack_channel_label!(" c0123456789 ")

    run_label_backfill

    permission = principal.slack_channel_permissions.reload.sole
    assert_equal "C0123456789", permission.channel_id
    assert_predicate permission, :upload_enabled
    assert_predicate permission, :download_enabled
    assert_predicate permission, :history_enabled
  end

  test "label backfill migration leaves existing slack permissions untouched" do
    principal = insert_principal_with_slack_channel_label!("C0123456789")
    SlackChannelPermission.create!(
      principal: principal,
      channel_id: "C0123456789",
      upload_enabled: true,
      download_enabled: false,
      history_enabled: false
    )

    run_label_backfill

    permission = principal.slack_channel_permissions.reload.sole
    assert_predicate permission, :upload_enabled
    assert_not permission.download_enabled
    assert_not permission.history_enabled
  end

  test "slack DM backfill migration creates permissions from slack user labels" do
    principal = insert_principal_with_slack_user_label!(" u0123456789 ")

    with_slack_channel_catalog(
      SlackChannelCatalog::Result.new(
        channels: [
          SlackChannelCatalog::Channel.new(id: "D0123456789", name: "U0123456789", private: true, im: true)
        ],
        error: nil,
        configured: true
      )
    ) do
      run_dm_backfill
    end

    permission = principal.slack_channel_permissions.reload.sole
    assert_equal "D0123456789", permission.channel_id
    assert_equal "U0123456789", permission.channel_name
    assert_predicate permission, :upload_enabled
    assert_predicate permission, :download_enabled
    assert_predicate permission, :history_enabled
  end

  test "slack DM backfill migration preserves existing permission flags" do
    principal = insert_principal_with_slack_user_label!("U0123456789")
    SlackChannelPermission.create!(
      principal: principal,
      channel_id: "D0123456789",
      channel_name: "U0123456789",
      upload_enabled: true,
      download_enabled: false,
      history_enabled: false
    )

    with_slack_channel_catalog(
      SlackChannelCatalog::Result.new(
        channels: [
          SlackChannelCatalog::Channel.new(id: "D0123456789", name: "U0123456789", private: true, im: true)
        ],
        error: nil,
        configured: true
      )
    ) do
      run_dm_backfill
    end

    permission = principal.slack_channel_permissions.reload.sole
    assert_equal "U0123456789", permission.channel_name
    assert_predicate permission, :upload_enabled
    assert_not permission.download_enabled
    assert_not permission.history_enabled
  end

  test "slack DM backfill migration no-ops when slack catalog is unavailable" do
    principal = insert_principal_with_slack_user_label!("U0123456789")

    with_slack_channel_catalog(
      SlackChannelCatalog::Result.new(channels: [], error: "SLACK_BOT_TOKEN is not configured.", configured: false)
    ) do
      run_dm_backfill
    end

    assert_empty principal.slack_channel_permissions.reload
  end

  private

  def run_label_backfill
    ActiveRecord::Migration.suppress_messages do
      BackfillSlackChannelPermissionsFromLabels.new.up
    end
  end

  def run_dm_backfill
    ActiveRecord::Migration.suppress_messages do
      BackfillSlackDmChannelPermissionsFromUserLabels.new.up
    end
  end

  def insert_principal_with_slack_channel_label!(channel_id)
    connection = Principal.connection
    labels = { Principal::SLACK_CHANNEL_ID_LABEL => channel_id }.to_json
    principal_id = connection.select_value(<<~SQL.squish)
      INSERT INTO principals (
        namespace,
        foreign_id,
        labels,
        created_by_id,
        created_at,
        updated_at
      )
      VALUES (
        #{connection.quote("migration-test")},
        #{connection.quote("legacy-label-#{SecureRandom.hex(6)}")},
        #{connection.quote(labels)}::jsonb,
        #{users(:acme_admin).id},
        CURRENT_TIMESTAMP,
        CURRENT_TIMESTAMP
      )
      RETURNING id
    SQL
    Principal.find(principal_id)
  end

  def insert_principal_with_slack_user_label!(user_id)
    connection = Principal.connection
    labels = { Principal::SLACK_USER_ID_LABEL => user_id }.to_json
    principal_id = connection.select_value(<<~SQL.squish)
      INSERT INTO principals (
        namespace,
        foreign_id,
        labels,
        created_by_id,
        created_at,
        updated_at
      )
      VALUES (
        #{connection.quote("migration-test")},
        #{connection.quote("legacy-dm-label-#{SecureRandom.hex(6)}")},
        #{connection.quote(labels)}::jsonb,
        #{users(:acme_admin).id},
        CURRENT_TIMESTAMP,
        CURRENT_TIMESTAMP
      )
      RETURNING id
    SQL
    Principal.find(principal_id)
  end

  def with_slack_channel_catalog(result)
    singleton = SlackChannelCatalog.singleton_class
    original = singleton.instance_method(:fetch)
    singleton.define_method(:fetch, -> { result })
    yield
  ensure
    singleton.define_method(:fetch, original)
  end
end
