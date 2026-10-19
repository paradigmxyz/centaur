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
