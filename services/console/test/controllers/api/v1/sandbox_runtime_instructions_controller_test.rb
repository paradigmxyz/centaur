require "test_helper"

module Api
  module V1
    class SandboxRuntimeInstructionsControllerTest < ActionDispatch::IntegrationTest
      setup do
        @proxy = proxies(:acme_proxy)
      end

      test "returns the currently published instructions to a valid sandbox" do
        version = OrganizationInstructionVersion.create!(
          content: "Search Notion for current company workstreams.",
          published_by: users(:acme_admin)
        )
        system_settings(:default).update!(published_organization_instruction_version: version)

        with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
          get "/api/v1/sandbox/runtime_instructions", headers: auth_headers(token_for(@proxy))
        end

        assert_response :ok
        assert_equal version.id.to_s, json_body.dig("data", "revision")
        assert_equal version.content, json_body.dig("data", "content")
        assert_equal Digest::SHA256.hexdigest(version.content), json_body.dig("data", "sha256")
        assert_equal "no-store", response.headers["Cache-Control"]
      end

      test "returns an empty unpublished state" do
        with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
          get "/api/v1/sandbox/runtime_instructions", headers: auth_headers(token_for(@proxy))
        end

        assert_response :ok
        assert_nil json_body.dig("data", "revision")
        assert_equal "", json_body.dig("data", "content")
      end

      test "rejects requests without a sandbox token" do
        get "/api/v1/sandbox/runtime_instructions"
        assert_response :unauthorized
      end

      private

      def auth_headers(token)
        { "Authorization" => "Bearer #{token}" }
      end

      def token_for(proxy)
        SandboxEntitlements::Jwt.encode_for_proxy(proxy)
      end

      def json_body
        JSON.parse(response.body)
      end
    end
  end
end
