require "test_helper"
require "securerandom"
require Rails.root.join("db/migrate/20260709223000_backfill_slack_channel_permissions_from_labels").to_s

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

  test "role permissions require exactly one grantee and are unique per channel" do
    role = roles(:acme_infra)
    permission = SlackChannelPermission.new(
      role: role,
      channel_id: " c0123456789 ",
      upload_enabled: true
    )

    assert_predicate permission, :valid?
    permission.save!
    assert_equal "C0123456789", permission.channel_id

    duplicate = role.slack_channel_permissions.new(
      channel_id: "C0123456789",
      history_enabled: true
    )
    assert_not duplicate.valid?
    assert_includes duplicate.errors[:channel_id], "has already been taken"

    missing = SlackChannelPermission.new(channel_id: "G9876543210", upload_enabled: true)
    assert_not missing.valid?
    assert_includes missing.errors[:base], "must reference exactly one of principal, role"

    both = SlackChannelPermission.new(
      principal: principals(:acme_channel),
      role: role,
      channel_id: "G9876543210",
      upload_enabled: true
    )
    assert_not both.valid?
    assert_includes both.errors[:base], "must reference exactly one of principal, role"
  end

  test "database requires exactly one grantee" do
    assert_raises ActiveRecord::StatementInvalid do
      SlackChannelPermission.transaction(requires_new: true) do
        SlackChannelPermission.insert_all!([
          {
            principal_id: principals(:acme_channel).id,
            role_id: roles(:acme_infra).id,
            channel_id: "C0123456789",
            upload_enabled: true,
            download_enabled: false,
            history_enabled: false,
            created_at: Time.current,
            updated_at: Time.current
          }
        ])
      end
    end
  end

  test "replace_for_role normalizes and merges duplicate rows" do
    role = roles(:acme_infra)

    SlackChannelPermission.replace_for_role!(
      role,
      [
        {
          channel_id: " c0123456789 ",
          channel_name: "general",
          upload_enabled: "1",
          download_enabled: "0",
          history_enabled: "0"
        },
        {
          channel_id: "C0123456789",
          channel_name: "ignored",
          upload_enabled: "0",
          download_enabled: "1",
          history_enabled: "1"
        }
      ]
    )

    permission = role.slack_channel_permissions.reload.sole
    assert_equal "C0123456789", permission.channel_id
    assert_equal "general", permission.channel_name
    assert_predicate permission, :upload_enabled
    assert_predicate permission, :download_enabled
    assert_predicate permission, :history_enabled
  end

  test "role permission changes invalidate every assigned principal" do
    role = roles(:acme_infra)
    principal_ids = role.principal_ids
    versions = Principal.where(id: principal_ids).pluck(:id, :sync_config_cache_version).to_h

    permission = role.slack_channel_permissions.create!(
      channel_id: "C0123456789",
      upload_enabled: true
    )
    Principal.where(id: principal_ids).find_each do |principal|
      assert_operator principal.sync_config_cache_version, :>, versions.fetch(principal.id)
    end

    versions = Principal.where(id: principal_ids).pluck(:id, :sync_config_cache_version).to_h
    permission.update!(history_enabled: true)
    Principal.where(id: principal_ids).find_each do |principal|
      assert_operator principal.sync_config_cache_version, :>, versions.fetch(principal.id)
    end

    versions = Principal.where(id: principal_ids).pluck(:id, :sync_config_cache_version).to_h
    permission.destroy!
    Principal.where(id: principal_ids).find_each do |principal|
      assert_operator principal.sync_config_cache_version, :>, versions.fetch(principal.id)
    end
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

  private

  def run_label_backfill
    ActiveRecord::Migration.suppress_messages do
      BackfillSlackChannelPermissionsFromLabels.new.up
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
end
