require "test_helper"

class PrincipalMergeTest < ActiveSupport::TestCase
  setup do
    @admin = users(:acme_admin)
    @source = Principal.create!(foreign_id: "slack-user-t0123456789-u0123456789", kind: "user",
                                labels: { "team" => "finance", "shared" => "source" }, created_by: @admin)
    @target = Principal.create!(foreign_id: "console-user-ada", kind: "user",
                                labels: { "shared" => "target" }, created_by: @admin)
  end

  test "moves grants, roles, Slack permissions, and proxies, then answers to the old foreign_id" do
    shared = static_secrets(:acme_staging_api_key)
    only_source = static_secrets(:acme_prod_api_key)
    [ @source, @target ].each { |principal| principal.grants.create!(static_secret: shared, created_by: @admin) }
    @source.grants.create!(static_secret: only_source, created_by: @admin)
    @source.principal_roles.create!(role: roles(:acme_infra))
    @source.slack_channel_permissions.create!(channel_id: "D0123456789", upload_enabled: true)
    @target.slack_channel_permissions.create!(channel_id: "D0123456789", history_enabled: true)
    proxy = Proxy.create!(name: "merge-proxy", principal: @source, requester_principal: @source,
                          bearer_token_hash: Digest::SHA256.hexdigest("merge"))

    PrincipalMerge.call(source: @source, target: @target)

    assert_not Principal.exists?(@source.id)
    assert_equal @target, Principal.resolve_foreign_id("slack-user-t0123456789-u0123456789")
    assert_equal [ shared, only_source ].sort_by(&:id), @target.granted_static_secrets.sort_by(&:id)
    assert_includes @target.roles, roles(:acme_infra)
    permission = @target.slack_channel_permissions.sole
    assert permission.upload_enabled
    assert permission.history_enabled
    assert_equal [ @target, @target ], [ proxy.reload.principal, proxy.requester_principal ]
    assert_equal({ "team" => "finance", "shared" => "target" }, @target.reload.labels.except(Principal::SANDBOX_REPO_CACHE_LABEL))
  end

  test "carries the source's own identities over to the target" do
    @source.identities.create!(foreign_id: "slack-user-legacy")

    PrincipalMerge.call(source: @source, target: @target)

    assert_equal %w[slack-user-legacy slack-user-t0123456789-u0123456789],
                 @target.identities.pluck(:foreign_id).sort
  end

  test "an identity cannot be reused as a principal's foreign_id" do
    PrincipalMerge.call(source: @source, target: @target)

    principal = Principal.new(foreign_id: "slack-user-t0123456789-u0123456789", kind: "user", created_by: @admin)
    assert_not principal.valid?
    assert principal.errors.of_kind?(:foreign_id, :taken)
  end
end
