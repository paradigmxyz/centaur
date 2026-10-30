require "test_helper"
require "tmpdir"
require "open3"


class Console::SshConnectionsControllerTest < ActionDispatch::IntegrationTest
  setup do
    post login_url, params: { email: users(:member_user).email, password: "password123456" }
    @private_key, @public_key = Dir.mktmpdir do |dir|
      path = File.join(dir, "synthetic")
      _out, _err, status = Open3.capture3("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", path)
      raise "synthetic key generation failed" unless status.success?
      [ File.read(path), File.read("#{path}.pub").split.first(2).join(" ") ]
    end
    @attributes = { name: "My SSH key", private_key: @private_key }
  end

  test "non-admin can enroll manage and disconnect only their own connection" do
    get console_integrations_url
    assert_select "a[href=?]", edit_console_ssh_connection_path, text: "Connect"
    patch console_ssh_connection_url, params: { ssh_connection: @attributes.merge(user_id: users(:acme_admin).id) }
    assert_redirected_to console_ssh_connection_path
    record = users(:member_user).reload.ssh_connection
    assert record
    assert_nil record.destination
    assert_nil record.username
    assert_nil record.host_key
    assert_equal SshKey.normalize(@private_key), record.private_key
    assert_nil users(:acme_admin).ssh_connection
    get console_ssh_connection_url
    assert_response :ok
    assert_select "textarea#ssh-public-key", text: @public_key
    assert_select "[role=status]", text: /SSH key saved/
    assert_no_match(/BEGIN OPENSSH PRIVATE KEY/, response.body)
    get edit_console_ssh_connection_url
    assert_response :ok
    assert_select "input[name='ssh_connection[destination]']", count: 0
    assert_select "textarea[name='ssh_connection[host_key]']", count: 0
    assert_select "textarea[name='ssh_connection[private_key]']", text: ""
    assert_select "select[name='ssh_connection[duration_hours]']", count: 0
    record.update_columns(expires_at: 1.minute.ago)
    patch console_ssh_connection_url, params: { ssh_connection: { name: "Updated", private_key: "", expires_at: 1.minute.ago } }
    assert_redirected_to console_ssh_connection_path
    assert_nil record.reload.expires_at
    assert record.configured?
    delete console_ssh_connection_url
    assert_redirected_to console_integrations_path
    assert record.reload.revoked?
    assert_nil record.private_key
  end

  test "live validation handles browser line endings without saving or echoing keys" do
    assert_no_difference "SshConnection.count" do
      post validate_key_console_ssh_connection_url, params: { private_key: @private_key.gsub("\n", "\r\n") }, as: :json
    end
    assert_response :ok
    assert response.parsed_body["valid"]
    assert_equal SshKey.fingerprint(@public_key), response.parsed_body["fingerprint"]
    assert_includes response.headers["Cache-Control"], "no-store"
    assert_no_match(/BEGIN OPENSSH PRIVATE KEY/, response.body)
    post validate_key_console_ssh_connection_url, params: { private_key: @public_key }, as: :json
    assert_response :unprocessable_entity
    assert_includes response.parsed_body["message"], "public key"
    assert_not response.parsed_body["valid"]
    assert_not_includes response.body, @public_key
    delete logout_url
    post validate_key_console_ssh_connection_url, params: { private_key: @private_key }, as: :json
    assert_response :redirect
  end

  test "validation never echoes a submitted private key" do
    patch console_ssh_connection_url, params: { ssh_connection: @attributes.merge(name: "") }
    assert_response :unprocessable_entity
    assert_select "textarea[name='ssh_connection[private_key]']", text: ""
    assert_select "textarea[name='ssh_connection[private_key]'][required]"
    assert_no_match(/BEGIN OPENSSH PRIVATE KEY/, response.body)
    assert_nil users(:member_user).reload.ssh_connection
  end

  test "other users cannot access a saved connection" do
    patch console_ssh_connection_url, params: { ssh_connection: @attributes }
    delete logout_url
    post login_url, params: { email: users(:globex_admin).email, password: "password123456" }
    get console_ssh_connection_url
    assert_redirected_to edit_console_ssh_connection_path
    delete console_ssh_connection_url
    assert users(:member_user).reload.ssh_connection.configured?
  end

  test "anonymous pending and disabled users cannot configure SSH" do
    delete logout_url
    get edit_console_ssh_connection_url
    assert_redirected_to login_path
    patch console_ssh_connection_url, params: { ssh_connection: @attributes }
    assert_redirected_to login_path
    %i[pending_user disabled_user].each do |fixture|
      post login_url, params: { email: users(fixture).email, password: "password123456" }
      patch console_ssh_connection_url, params: { ssh_connection: @attributes }
      assert_response :redirect
      assert_nil users(fixture).reload.ssh_connection
      delete logout_url
    end
  end
end
