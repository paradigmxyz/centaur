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

    get console_secrets_url(type: "ssh_key")
    assert_response :ok
    assert_select "td", text: /Member key/

    get console_secret_url("ssh_key", key.oid)
    assert_response :ok
    assert_no_match(/BEGIN OPENSSH PRIVATE KEY/, response.body)

    post console_principal_grant_secret_url(principals(:acme_channel).oid), params: { grantable: "ssh_key:#{key.oid}" }
    get console_principal_url(principals(:acme_channel).oid)
    assert_response :ok
    assert_select "a[href=?]", console_secret_path("ssh_key", key.oid)
  end
end
