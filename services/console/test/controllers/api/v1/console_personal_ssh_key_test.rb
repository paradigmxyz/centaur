require "test_helper"

module Api
  module V1
    # The private key is write-only: no response may contain it.
    class ConsolePersonalSshKeyTest < ActionDispatch::IntegrationTest
      PRIVATE_KEY_MARKER = /PRIVATE KEY/

      setup do
        @member = users(:member_user)
        sign_in_console(@member)
        @private_key, @public_key = synthetic_ssh_key
      end

      def save_key(name: "My SSH key", private_key: @private_key, **extra)
        put api_v1_me_ssh_key_path, params: { data: { name: name, private_key: private_key }.merge(extra) },
                                    headers: csrf_headers, as: :json
      end

      test "ssh key endpoints are open to active users only" do
        assert_active_user_api :get, api_v1_me_ssh_key_path
        assert_active_user_api :put, api_v1_me_ssh_key_path, params: { data: { name: "Mine", private_key: @private_key } }
        assert_active_user_api :delete, api_v1_me_ssh_key_path
        assert_active_user_api :post, validate_api_v1_me_ssh_key_path, params: { data: { private_key: @private_key } }
        assert_equal 0, SshKeySecret.where.not(owner_id: nil).count
      end

      test "a member saves, views, edits, and deletes only their own key" do
        console = Principal.create!(foreign_id: "ssh-console-member", kind: "console_user",
                                    console_user: @member, created_by: users(:acme_admin))
        get api_v1_me_ssh_key_path
        assert_response :ok
        assert_equal({ "saved" => false, "name" => "My SSH key", "public_key" => nil, "grants" => [] },
                     json_body["data"].slice("saved", "name", "public_key", "grants"))

        save_key(owner_id: users(:acme_admin).id)
        assert_response :ok
        assert_includes response.headers["Cache-Control"], "no-store"
        assert_no_match PRIVATE_KEY_MARKER, response.body
        key = @member.reload.personal_ssh_key
        assert key
        assert_equal @member, key.owner
        assert_nil users(:acme_admin).personal_ssh_key
        assert console.grants.exists?(ssh_key_secret: key)

        get api_v1_me_ssh_key_path
        assert_response :ok
        data = json_body["data"]
        assert data["saved"]
        assert data["has_private_key"]
        assert_equal key.oid, data["id"]
        assert_equal @public_key.split.first(2).join(" "), data["public_key"]
        assert_equal SshKey.fingerprint(@public_key), data["fingerprint"]
        grant = data["grants"].find { |g| g["label"] == "ssh-console-member" }
        assert_equal({ "grantee_type" => "principal", "grantee_id" => console.oid, "principal_kind" => "console_user" },
                     grant.slice("grantee_type", "grantee_id", "principal_kind"))
        assert_no_match PRIVATE_KEY_MARKER, response.body

        save_key(name: "Renamed", private_key: "")
        assert_response :ok
        assert_equal "Renamed", key.reload.name
        assert_equal SshKey.normalize(@private_key), key.key_source.secret

        delete api_v1_me_ssh_key_path, headers: csrf_headers
        assert_response :no_content
        assert_nil @member.reload.personal_ssh_key
        assert_not console.grants.where.not(ssh_key_secret_id: nil).exists?

        delete api_v1_me_ssh_key_path, headers: csrf_headers
        assert_response :no_content, "deleting a missing key is a no-op"
      end

      test "saving again restores grants a failed enrollment never created" do
        console = Principal.create!(foreign_id: "ssh-console-repair", kind: "console_user",
                                    console_user: @member, created_by: users(:acme_admin))
        skipped = Object.new
        def skipped.apply_for_user(_user) = 0
        def skipped.apply_for_principal(_principal) = false
        key = PersonalSshKeyGrants.stub(:new, skipped) do
          save_key(name: "Mine")
          @member.reload.personal_ssh_key
        end
        assert key
        assert_not console.grants.exists?(ssh_key_secret: key), "enrollment was skipped"

        save_key(name: "Mine", private_key: "")
        assert_response :ok
        assert console.grants.exists?(ssh_key_secret: key)
      end

      test "live validation handles browser line endings without saving or echoing keys" do
        assert_no_difference "SshKeySecret.count" do
          post validate_api_v1_me_ssh_key_path, params: { data: { private_key: @private_key.gsub("\n", "\r\n") } },
                                                headers: csrf_headers, as: :json
        end
        assert_response :ok
        assert json_body.dig("data", "valid")
        assert_equal "Valid private key. Ready to save.", json_body.dig("data", "message")
        assert_equal SshKey.fingerprint(@public_key), json_body.dig("data", "fingerprint")
        assert_includes response.headers["Cache-Control"], "no-store"
        assert_no_match PRIVATE_KEY_MARKER, response.body

        post validate_api_v1_me_ssh_key_path, params: { data: { private_key: @public_key } },
                                              headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert_includes json_body.dig("error", "message"), "public key"
        assert_equal false, json_body.dig("error", "details", "valid")
        assert_includes response.headers["Cache-Control"], "no-store"
        assert_not_includes response.body, @public_key
      end

      test "validation errors never echo a submitted private key" do
        save_key(name: "")
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "name").present?
        assert_no_match PRIVATE_KEY_MARKER, response.body
        assert_nil @member.reload.personal_ssh_key

        broken = "-----BEGIN OPENSSH PRIVATE KEY-----\nnot-a-real-key-body\n-----END OPENSSH PRIVATE KEY-----\n"
        save_key(private_key: broken)
        assert_response :unprocessable_entity
        assert json_body.dig("error", "details", "private_key").present?
        assert_no_match PRIVATE_KEY_MARKER, response.body
        assert_not_includes response.body, "not-a-real-key-body"

        post validate_api_v1_me_ssh_key_path, params: { data: { private_key: broken } },
                                              headers: csrf_headers, as: :json
        assert_response :unprocessable_entity
        assert_no_match PRIVATE_KEY_MARKER, response.body
        assert_not_includes response.body, "not-a-real-key-body"
      end

      test "another user cannot see or delete a saved key" do
        save_key(name: "Mine")
        assert_response :ok

        sign_in_console(users(:globex_admin))
        get api_v1_me_ssh_key_path
        assert_response :ok
        assert_equal false, json_body.dig("data", "saved"), "an admin only sees their own key here"
        delete api_v1_me_ssh_key_path, headers: csrf_headers
        assert_response :no_content
        assert @member.reload.personal_ssh_key
      end

      test "admins see personal keys as ordinary grantable secrets" do
        save_key(name: "Member key")
        key = @member.reload.personal_ssh_key
        sign_in_console(users(:acme_admin))

        get api_v1_secrets_path(kind: "ssh_key")
        assert_response :ok
        assert_includes json_body["data"].map { |s| s["name"] }, "Member key"
        assert_no_match PRIVATE_KEY_MARKER, response.body

        get api_v1_ssh_key_secret_path(key.oid)
        assert_response :ok
        assert_no_match PRIVATE_KEY_MARKER, response.body

        principal = principals(:acme_channel)
        post api_v1_grants_path, params: { data: { principal_id: principal.oid, ssh_key_secret_id: key.oid } },
             headers: csrf_headers, as: :json
        assert_response :created
        get api_v1_principal_grants_path(principal.oid)
        grant = json_body["data"].find { |g| g["ssh_key_secret_id"] == key.oid }
        assert_equal({ "kind" => "ssh_key", "id" => key.oid }, grant["grantable"].slice("kind", "id"))
        assert_no_match PRIVATE_KEY_MARKER, response.body

        sign_in_console(@member)
        get api_v1_me_ssh_key_path
        labels = json_body.dig("data", "grants").map { |g| [ g["grantee_type"], g["label"] ] }
        assert_includes labels, [ "principal", principal.name.presence || principal.foreign_id ]
      end
    end
  end
end
