require "test_helper"

module Console
  class RolesControllerTest < ActionDispatch::IntegrationTest
    include ActiveJob::TestHelper

    setup do
      @operator = users(:acme_admin)
      post login_url, params: { email: @operator.email, password: "password123456" }
    end

    test "redirects to login when signed out" do
      delete logout_url
      get console_roles_url
      assert_redirected_to login_path
    end

    test "index lists roles and links to new" do
      role = roles(:acme_infra)
      get console_roles_url
      assert_response :ok
      assert_select "h1", text: "Roles"
      assert_select "a[href=?]", new_console_role_path, text: "Add Role"
      assert_select "tr[onclick=?]", "window.location='#{console_role_path(role.oid)}'"
    end

    test "show renders role details and editable secret grants" do
      role = roles(:acme_infra)
      grant = grants(:acme_infra_prod_api_key)
      get console_role_url(role.oid)
      assert_response :ok
      assert_select "h1", text: role.name
      assert_select "a[href=?]", edit_console_role_path(role.oid), text: "Edit"
      assert_select "form[action=?]", slack_channel_permissions_console_role_path(role.oid)
      assert_select "a[href=?]", console_secret_path("static", static_secrets(:acme_prod_api_key).oid)
      assert_select "form[action=?]", grant_secret_console_role_path(role.oid) do
        assert_select "select[name=grantable][aria-label=?]", "Secret to grant"
        assert_select "option[value=?]", "static:#{static_secrets(:acme_staging_api_key).oid}"
        assert_select "option[value=?]", "static:#{static_secrets(:acme_prod_api_key).oid}", count: 0
        assert_select "option[value=?]", "static:#{static_secrets(:globex_prod_secret).oid}", count: 0
      end
      assert_select "form[action=?]", revoke_grant_console_role_path(role.oid, grant.oid) do
        assert_select "button[type=submit]", "Revoke"
      end
    end

    test "update_slack_channel_permissions stores role permissions" do
      role = roles(:acme_infra)

      patch slack_channel_permissions_console_role_url(role.oid),
            params: {
              role: {
                slack_channel_permissions_attributes: {
                  "0" => {
                    channel_id: "C0123456789",
                    upload_enabled: "1",
                    download_enabled: "0",
                    history_enabled: "1"
                  }
                }
              }
            }

      assert_redirected_to console_role_path(role.oid)
      permission = role.slack_channel_permissions.reload.sole
      assert_equal "C0123456789", permission.channel_id
      assert_predicate permission, :upload_enabled
      assert_not permission.download_enabled
      assert_predicate permission, :history_enabled
    end

    test "role permission form renders existing channels as immutable" do
      role = roles(:acme_infra)
      permission = role.slack_channel_permissions.create!(
        channel_id: "C0123456789",
        upload_enabled: true
      )
      catalog = SlackChannelCatalog::Result.new(
        channels: [ SlackChannelCatalog::Channel.new(id: permission.channel_id, name: "general", private: false) ],
        error: nil,
        configured: true
      )

      with_slack_channel_catalog(catalog) { get console_role_url(role.oid) }
      assert_response :ok
      assert_select "tbody tr" do
        assert_select "td", text: /#general/
        assert_select "select[name$='[channel_id]']", count: 0
        assert_select "input[name$='[channel_id]']", count: 0
      end

      patch slack_channel_permissions_console_role_url(role.oid),
            params: {
              role: {
                slack_channel_permissions_attributes: {
                  "0" => {
                    id: permission.id,
                    upload_enabled: "0",
                    download_enabled: "1",
                    history_enabled: "0"
                  }
                }
              }
            }

      assert_redirected_to console_role_path(role.oid)
      permission = role.slack_channel_permissions.find_by!(channel_id: "C0123456789")
      assert_equal "C0123456789", permission.channel_id
      assert_not permission.upload_enabled
      assert_predicate permission, :download_enabled
      assert_not permission.history_enabled
    end

    test "update_slack_channel_permissions rejects channel id changes" do
      role = roles(:acme_infra)
      permission = role.slack_channel_permissions.create!(
        channel_id: "C0123456789",
        upload_enabled: true
      )

      patch slack_channel_permissions_console_role_url(role.oid),
            params: {
              role: {
                slack_channel_permissions_attributes: {
                  "0" => {
                    id: permission.id,
                    channel_id: "G9876543210",
                    upload_enabled: "1"
                  }
                }
              }
            }

      assert_redirected_to console_role_path(role.oid)
      assert_equal "Slack channels cannot be changed after creation.", flash[:alert]
      assert_equal "C0123456789", permission.reload.channel_id
    end

    test "update_slack_channel_permissions bumps versions without warm jobs" do
      role = roles(:acme_infra)
      first = role.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)
      second = role.slack_channel_permissions.create!(channel_id: "G9876543210", download_enabled: true)
      versions = Principal.where(id: role.principal_ids).pluck(:id, :sync_config_cache_version).to_h
      clear_enqueued_jobs

      assert_no_enqueued_jobs only: PrincipalSyncConfigSnapshotWarmJob do
        patch slack_channel_permissions_console_role_url(role.oid),
              params: {
                role: {
                  slack_channel_permissions_attributes: {
                    "0" => {
                      id: first.id,
                      upload_enabled: "0",
                      download_enabled: "1",
                      history_enabled: "0"
                    },
                    "1" => {
                      id: second.id,
                      _destroy: "1"
                    },
                    "2" => {
                      channel_id: "C2222222222",
                      upload_enabled: "1",
                      download_enabled: "0",
                      history_enabled: "1"
                    }
                  }
                }
              }
      end

      assert_redirected_to console_role_path(role.oid)
      assert_equal %w[C0123456789 C2222222222], role.slack_channel_permissions.reload.pluck(:channel_id).sort
      Principal.where(id: role.principal_ids).find_each do |principal|
        assert_equal versions.fetch(principal.id) + 1, principal.sync_config_cache_version
      end
    end

    test "update_slack_channel_permissions skips unchanged submissions" do
      role = roles(:acme_infra)
      permission = role.slack_channel_permissions.create!(channel_id: "C0123456789", upload_enabled: true)
      versions = Principal.where(id: role.principal_ids).pluck(:id, :sync_config_cache_version).to_h
      clear_enqueued_jobs

      assert_no_enqueued_jobs only: PrincipalSyncConfigSnapshotWarmJob do
        patch slack_channel_permissions_console_role_url(role.oid),
              params: {
                role: {
                  slack_channel_permissions_attributes: {
                    "0" => {
                      id: permission.id,
                      upload_enabled: "1",
                      download_enabled: "0",
                      history_enabled: "0"
                    }
                  }
                }
              }
      end

      assert_redirected_to console_role_path(role.oid)
      assert_equal "Updated Slack channel permissions.", flash[:notice]
      assert_equal [ permission.id ], role.slack_channel_permissions.reload.pluck(:id)
      Principal.where(id: role.principal_ids).find_each do |principal|
        assert_equal versions.fetch(principal.id), principal.sync_config_cache_version
      end
    end

    test "new and edit render forms" do
      role = roles(:acme_infra)
      get new_console_role_url
      assert_response :ok
      assert_select "form[action=?]", console_roles_path
      assert_select "input[name=?]", "role[namespace]"
      assert_select "input[name=?]", "role[foreign_id]"

      get edit_console_role_url(role.oid)
      assert_response :ok
      assert_select "form[action=?]", console_role_path(role.oid)
      assert_select "input[name=?]", "role[name]"
      assert_select "input[name=?]", "role[namespace]", count: 0
      assert_select "input[name=?]", "role[foreign_id]", count: 0
    end

    test "create persists role identity and labels" do
      assert_difference -> { Role.count }, 1 do
        post console_roles_url, params: {
          role: { namespace: "acme", foreign_id: "payments", name: "Payments" },
          labels: { "0" => { key: "team", value: "finance" } }
        }
      end

      role = Role.find_by!(namespace: "acme", foreign_id: "payments")
      assert_redirected_to console_role_path(role.oid)
      assert_equal "Payments", role.name
      assert_equal({ "team" => "finance" }, role.labels)
    end

    test "create rejects invalid role without writing" do
      assert_no_difference -> { Role.count } do
        post console_roles_url, params: {
          role: { namespace: "bad namespace", foreign_id: "broken", name: "Broken" }
        }
      end
      assert_response :unprocessable_entity
    end

    test "update changes mutable fields only" do
      role = roles(:acme_infra)
      patch console_role_url(role.oid), params: {
        role: { namespace: "globex", foreign_id: "changed", name: "Infrastructure" },
        labels: { "0" => { key: "kind", value: "platform" } }
      }

      assert_redirected_to console_role_path(role.oid)
      role.reload
      assert_equal "Infrastructure", role.name
      assert_equal "acme", role.namespace
      assert_equal "infra", role.foreign_id
      assert_equal({ "kind" => "platform" }, role.labels)
    end

    test "grant_secret grants a same-namespace secret at the default role priority" do
      role = roles(:acme_admin_role)
      secret = static_secrets(:acme_staging_api_key)

      assert_difference -> { role.grants.count }, 1 do
        post grant_secret_console_role_url(role.oid), params: { grantable: "static:#{secret.oid}" }
      end

      assert_redirected_to console_role_path(role.oid)
      grant = role.grants.find_by(static_secret: secret)
      assert_not_nil grant
      assert_equal Grant::DEFAULT_ROLE_PRIORITY, grant.priority
    end

    test "grant_secret is idempotent" do
      role = roles(:acme_infra)
      secret = static_secrets(:acme_prod_api_key)

      assert_no_difference -> { role.grants.count } do
        post grant_secret_console_role_url(role.oid), params: { grantable: "static:#{secret.oid}" }
      end

      assert_redirected_to console_role_path(role.oid)
    end

    test "grant_secret rejects a secret from another namespace" do
      role = roles(:acme_admin_role)
      secret = static_secrets(:globex_prod_secret)

      assert_no_difference -> { Grant.count } do
        post grant_secret_console_role_url(role.oid), params: { grantable: "static:#{secret.oid}" }
      end

      assert_redirected_to console_role_path(role.oid)
      assert_equal "Secret must be in the same namespace as the role.", flash[:alert]
    end

    test "revoke_grant removes the role grant" do
      role = roles(:acme_infra)
      grant = grants(:acme_infra_prod_api_key)

      assert_difference -> { role.grants.count }, -1 do
        delete revoke_grant_console_role_url(role.oid, grant.oid)
      end

      assert_redirected_to console_role_path(role.oid)
      assert_not Grant.exists?(grant.id)
    end

    test "unknown role returns 404" do
      get console_role_url("role_missing")
      assert_response :not_found
    end

    private

    def with_slack_channel_catalog(catalog)
      singleton = SlackChannelCatalog.singleton_class
      original = singleton.instance_method(:fetch)
      singleton.define_method(:fetch) { catalog }
      yield
    ensure
      singleton.define_method(:fetch, original)
    end
  end
end
