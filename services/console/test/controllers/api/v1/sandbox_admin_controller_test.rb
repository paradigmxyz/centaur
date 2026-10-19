require "test_helper"

class Api::V1::SandboxAdminControllerTest < ActionDispatch::IntegrationTest
  setup do
    @channel = principals(:acme_channel)
    @role = roles(:acme_admin_role)
  end

  test "an admin's Slack DM sandbox finds a channel principal and changes its roles" do
    proxy = dm_proxy_for(users(:acme_admin))

    with_token(proxy) do |headers|
      get "/api/v1/sandbox/admin/principals", params: { slack_channel_id: "c0123456789" }, headers: headers
      assert_response :ok
      assert_equal [ @channel.oid ], json_body.fetch("data").map { |principal| principal["id"] }

      get "/api/v1/sandbox/admin/roles/lookup/#{@role.foreign_id}", headers: headers
      assert_response :ok
      role_id = json_body.dig("data", "id")

      post "/api/v1/sandbox/admin/principals/#{@channel.oid}/roles",
           params: { data: { role_id: role_id } }, headers: headers, as: :json
      assert_response :created
      assert_includes @channel.reload.roles, @role

      delete "/api/v1/sandbox/admin/principals/#{@channel.oid}/roles/#{role_id}", headers: headers
      assert_response :no_content
      assert_not_includes @channel.reload.roles, @role
    end
  end

  test "an admin's Slack DM sandbox finds a secret and grants it directly to a principal" do
    secret = static_secrets(:acme_staging_api_key)

    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      get "/api/v1/sandbox/admin/secrets", params: { q: secret.name }, headers: headers
      assert_response :ok
      listed = json_body.fetch("data").find { |row| row["id"] == secret.oid }
      assert_equal "static_secret_id", listed["grant_key"]
      assert_equal %w[description foreign_id grant_key id labels name], listed.keys.sort

      post "/api/v1/sandbox/admin/grants",
           params: { data: { principal_id: @channel.oid, static_secret_id: secret.oid } }, headers: headers, as: :json
      assert_response :created
      grant_id = json_body.dig("data", "id")
      assert_includes @channel.granted_static_secrets, secret

      get "/api/v1/sandbox/admin/principals/#{@channel.oid}/grants", headers: headers
      assert_includes json_body.fetch("data").map { |grant| grant["id"] }, grant_id

      delete "/api/v1/sandbox/admin/grants/#{grant_id}", headers: headers
      assert_response :no_content
      assert_not_includes @channel.reload.granted_static_secrets, secret
    end
  end

  test "sandbox admin grants cannot change role grants" do
    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      post "/api/v1/sandbox/admin/grants",
           params: { data: { role_id: @role.oid, static_secret_id: static_secrets(:acme_staging_api_key).oid } },
           headers: headers, as: :json
      assert_response :forbidden

      delete "/api/v1/sandbox/admin/grants/#{grants(:acme_infra_prod_api_key).oid}", headers: headers
      assert_response :not_found
    end
    assert Grant.exists?(grants(:acme_infra_prod_api_key).id)
  end

  test "an admin's Slack DM sandbox changes Slack permissions and sandbox capabilities" do
    @channel.update!(sandbox_sessions_read_enabled: false, sandbox_workflows_read_enabled: true)

    with_token(dm_proxy_for(users(:acme_admin))) do |headers|
      post "/api/v1/sandbox/admin/principals/#{@channel.oid}/slack_channel_permissions",
           params: { data: { channel_id: "C0999999999", upload_enabled: false } }, headers: headers, as: :json
      assert_response :created
      permission = @channel.slack_channel_permissions.find_by!(channel_id: "C0999999999")
      assert_not permission.upload_enabled
      assert permission.history_enabled

      delete "/api/v1/sandbox/admin/principals/#{@channel.oid}/slack_channel_permissions/c0999999999",
             headers: headers
      assert_response :no_content
      assert_not @channel.slack_channel_permissions.exists?(channel_id: "C0999999999")

      patch "/api/v1/sandbox/admin/principals/#{@channel.oid}/sandbox_access",
            params: { data: { sandbox_sessions_read_enabled: true } }, headers: headers, as: :json
      assert_response :ok
    end

    @channel.reload
    assert @channel.sandbox_sessions_read_enabled
    assert @channel.sandbox_workflows_read_enabled
    assert_equal "all", @channel.sandbox_repo_cache
  end

  test "sandboxes not linked to an active admin cannot change roles" do
    proxies = [
      dm_proxy_for(users(:member_user)),
      dm_proxy_for(users(:acme_admin).tap { |user| user.update!(status: :disabled) }),
      Proxy.create!(name: "channel-proxy", principal: @channel, bearer_token_hash: Digest::SHA256.hexdigest("channel"))
    ]

    proxies.each do |proxy|
      with_token(proxy) do |headers|
        post "/api/v1/sandbox/admin/principals/#{@channel.oid}/roles",
             params: { data: { role_id: @role.oid } }, headers: headers, as: :json
      end
      assert_response :forbidden
    end
    assert_not_includes @channel.reload.roles, @role
  end

  test "admin endpoints reject requests without a sandbox token" do
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      get "/api/v1/sandbox/admin/roles", headers: { "Authorization" => "Bearer iak_acme-ci-token" }
    end

    assert_response :unauthorized
  end

  private

  def dm_proxy_for(user)
    principal = Principal.create!(
      foreign_id: "slack-user-#{user.id}",
      name: "DM #{user.email}",
      kind: :slack_dm,
      console_user: user,
      created_by: users(:acme_admin)
    )
    Proxy.create!(
      name: "dm-proxy-#{user.id}",
      principal: principal,
      bearer_token_hash: Digest::SHA256.hexdigest("dm-#{user.id}")
    )
  end

  def with_token(proxy)
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      yield "Authorization" => "Bearer #{SandboxEntitlements::Jwt.encode_for_proxy(proxy)}"
    end
  end

  def json_body
    JSON.parse(response.body)
  end
end
