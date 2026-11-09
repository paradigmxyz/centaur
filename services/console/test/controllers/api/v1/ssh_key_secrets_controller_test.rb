require "test_helper"

module Api
  module V1
    class SshKeySecretsControllerTest < ActionDispatch::IntegrationTest
      ACME_TOKEN = "iak_acme-ci-token".freeze

      def auth_headers
        { "Authorization" => "Bearer #{ACME_TOKEN}", "Content-Type" => "application/json" }
      end

      def json_body
        JSON.parse(response.body)
      end

      test "creates an inline key, never returns it, and grants it like any secret" do
        private_key, public_key = synthetic_ssh_key
        post api_v1_ssh_key_secrets_url, headers: auth_headers, params: {
          data: { foreign_id: "team-dev-key", name: "Team dev key",
                  private_key: { source_type: "control_plane", secret: private_key } }
        }.to_json
        assert_response :created
        data = json_body["data"]
        assert_equal public_key, data["public_key"]
        assert_equal "control_plane", data.dig("private_key", "source_type")
        assert_not data["personal"]
        assert_no_match(/BEGIN OPENSSH PRIVATE KEY/, response.body)

        post api_v1_grants_url, headers: auth_headers, params: {
          data: { role_id: roles(:acme_infra).oid, ssh_key_secret_id: data["id"] }
        }.to_json
        assert_response :created
        assert_equal data["id"], json_body.dig("data", "ssh_key_secret_id")

        get lookup_api_v1_ssh_key_secrets_url(foreign_id: "team-dev-key"), headers: auth_headers
        assert_response :ok
        assert_equal data["id"], json_body.dig("data", "id")
      end

      test "an external source requires a public key" do
        put api_v1_ssh_key_secret_url("team-env-key"), headers: auth_headers, params: {
          data: { name: "Env key", private_key: { source_type: "env", config: { var: "TEAM_SSH_KEY" } } }
        }.to_json
        assert_response :unprocessable_entity

        put api_v1_ssh_key_secret_url("team-env-key"), headers: auth_headers, params: {
          data: { name: "Env key", public_key: synthetic_ssh_key.last,
                  private_key: { source_type: "env", config: { var: "TEAM_SSH_KEY" } } }
        }.to_json
        assert_response :created
        assert_equal({ "var" => "TEAM_SSH_KEY" }, json_body.dig("data", "private_key", "config"))
      end

      test "personal keys are owner-managed but an operator can delete them" do
        key = users(:member_user).create_personal_ssh_key!(
          name: "Mine", created_by: users(:member_user), private_key: synthetic_ssh_key.first
        )
        put api_v1_ssh_key_secret_url(key.oid), headers: auth_headers, params: {
          data: { name: "Taken", private_key: { source_type: "control_plane", secret: synthetic_ssh_key.first } }
        }.to_json
        assert_response :forbidden
        assert_equal "Mine", key.reload.name

        get api_v1_ssh_key_secret_url(key.oid), headers: auth_headers
        assert json_body.dig("data", "personal")
        delete api_v1_ssh_key_secret_url(key.oid), headers: auth_headers
        assert_response :no_content
        assert_nil SshKeySecret.find_by(id: key.id)
      end
    end
  end
end
