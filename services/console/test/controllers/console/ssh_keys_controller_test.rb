require "test_helper"

class Console::SshKeysControllerTest < ActionDispatch::IntegrationTest
  setup do
    post login_url, params: { email: users(:member_user).email, password: "password123456" }
    @private_key, @public_key = synthetic_ssh_key
  end

  test "a member saves, views, edits, and deletes only their own key" do
    console = Principal.create!(foreign_id: "ssh-console-member", kind: "console_user",
                                console_user: users(:member_user), created_by: users(:acme_admin))
    get console_integrations_url
    assert_select "a[href=?]", edit_console_ssh_key_path, text: "Connect"

    patch console_ssh_key_url, params: { ssh_key: { name: "My SSH key", private_key: @private_key, owner_id: users(:acme_admin).id } }
    assert_redirected_to console_ssh_key_path
    key = users(:member_user).reload.personal_ssh_key
    assert key
    assert_equal users(:member_user), key.owner
    assert_nil users(:acme_admin).personal_ssh_key
    assert console.grants.exists?(ssh_key_secret: key)

    get console_ssh_key_url
    assert_response :ok
    assert_select "textarea#ssh-public-key", text: @public_key
    assert_select "li", text: /ssh-console-member/
    assert_no_match(/BEGIN OPENSSH PRIVATE KEY/, response.body)

    get edit_console_ssh_key_url
    assert_select "textarea[name='ssh_key[private_key]']", text: ""
    assert_select "textarea[name='ssh_key[private_key]'][required]", count: 0

    patch console_ssh_key_url, params: { ssh_key: { name: "Renamed", private_key: "" } }
    assert_redirected_to console_ssh_key_path
    assert_equal "Renamed", key.reload.name
    assert_equal SshKey.normalize(@private_key), key.key_source.secret

    delete console_ssh_key_url
    assert_redirected_to console_integrations_path
    assert_nil users(:member_user).reload.personal_ssh_key
    assert_not console.grants.where.not(ssh_key_secret_id: nil).exists?
  end

  test "saving again restores grants a failed enrollment never created" do
    console = Principal.create!(foreign_id: "ssh-console-repair", kind: "console_user",
                                console_user: users(:member_user), created_by: users(:acme_admin))
    skipped = Object.new
    def skipped.apply_for_user(_user) = 0
    def skipped.apply_for_principal(_principal) = false
    key = PersonalSshKeyGrants.stub(:new, skipped) do
      patch console_ssh_key_url, params: { ssh_key: { name: "Mine", private_key: @private_key } }
      users(:member_user).reload.personal_ssh_key
    end
    assert key
    assert_not console.grants.exists?(ssh_key_secret: key), "enrollment was skipped"

    patch console_ssh_key_url, params: { ssh_key: { name: "Mine", private_key: "" } }
    assert_redirected_to console_ssh_key_path
    assert console.grants.exists?(ssh_key_secret: key)
  end

  test "live validation handles browser line endings without saving or echoing keys" do
    assert_no_difference "SshKeySecret.count" do
      post validate_key_console_ssh_key_url, params: { private_key: @private_key.gsub("\n", "\r\n") }, as: :json
    end
    assert_response :ok
    assert response.parsed_body["valid"]
    assert_equal SshKey.fingerprint(@public_key), response.parsed_body["fingerprint"]
    assert_includes response.headers["Cache-Control"], "no-store"
    assert_no_match(/BEGIN OPENSSH PRIVATE KEY/, response.body)

    post validate_key_console_ssh_key_url, params: { private_key: @public_key }, as: :json
    assert_response :unprocessable_entity
    assert_includes response.parsed_body["message"], "public key"
    assert_not_includes response.body, @public_key
  end

  test "validation errors never echo a submitted private key" do
    patch console_ssh_key_url, params: { ssh_key: { name: "", private_key: @private_key } }
    assert_response :unprocessable_entity
    assert_select "textarea[name='ssh_key[private_key]']", text: ""
    assert_select "textarea[name='ssh_key[private_key]'][required]"
    assert_no_match(/BEGIN OPENSSH PRIVATE KEY/, response.body)
    assert_nil users(:member_user).reload.personal_ssh_key
  end

  test "another user cannot see or delete a saved key" do
    patch console_ssh_key_url, params: { ssh_key: { name: "Mine", private_key: @private_key } }
    delete logout_url
    post login_url, params: { email: users(:globex_admin).email, password: "password123456" }
    get console_ssh_key_url
    assert_redirected_to edit_console_ssh_key_path
    delete console_ssh_key_url
    assert users(:member_user).reload.personal_ssh_key
  end

  test "anonymous, pending, and disabled users cannot configure SSH" do
    delete logout_url
    get edit_console_ssh_key_url
    assert_redirected_to login_path
    post validate_key_console_ssh_key_url, params: { private_key: @private_key }, as: :json
    assert_response :redirect
    %i[pending_user disabled_user].each do |fixture|
      post login_url, params: { email: users(fixture).email, password: "password123456" }
      patch console_ssh_key_url, params: { ssh_key: { name: "Mine", private_key: @private_key } }
      assert_response :redirect
      assert_nil users(fixture).reload.personal_ssh_key
      delete logout_url
    end
  end

  test "admins see personal keys as ordinary grantable secrets" do
    patch console_ssh_key_url, params: { ssh_key: { name: "Member key", private_key: @private_key } }
    key = users(:member_user).reload.personal_ssh_key
    delete logout_url
    post login_url, params: { email: users(:acme_admin).email, password: "password123456" }

    get api_v1_secrets_path(kind: "ssh_key")
    assert_response :ok
    assert_includes response.parsed_body["data"].map { |s| s["name"] }, "Member key"

    get api_v1_secret_detail_path("ssh_key", key.oid)
    assert_response :ok
    assert_equal false, response.parsed_body.dig("data", "editable")
    assert_no_match(/BEGIN OPENSSH PRIVATE KEY/, response.body)

    principal = principals(:acme_channel)
    post api_v1_grants_path, params: { data: { principal_id: principal.oid, ssh_key_secret_id: key.oid } },
         headers: csrf_headers, as: :json
    assert_response :created
    get api_v1_principal_grants_path(principal.oid)
    grant = response.parsed_body["data"].find { |g| g["ssh_key_secret_id"] == key.oid }
    assert_equal({ "kind" => "ssh_key", "id" => key.oid }, grant["grantable"].slice("kind", "id"))
    assert_no_match(/BEGIN OPENSSH PRIVATE KEY/, response.body)
  end
end
