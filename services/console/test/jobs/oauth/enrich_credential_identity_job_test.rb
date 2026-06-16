require "test_helper"

module Oauth
  class EnrichCredentialIdentityJobTest < ActiveJob::TestCase
    setup do
      Oauth::EnrichCredentialIdentityJob.slack_api_http = nil
    end

    teardown do
      Oauth::EnrichCredentialIdentityJob.slack_api_http = nil
    end

    def slack_credential(**overrides)
      app = oauth_apps(:acme_slack)
      app.update!(client_secret: "slack-secret")
      BrokerCredential.create!({
        namespace: "acme",
        foreign_id: "slack-slack-u12345",
        name: "Slack – U12345",
        token_endpoint: Oauth::Providers::Slack::TOKEN_ENDPOINT,
        oauth_app: app,
        provider_subject: "U12345",
        access_token: "AT",
        refresh_token: "RT",
        scopes: %w[users:read users:read.email channels:history]
      }.merge(overrides))
    end

    def wrap_credential(credential, name: "#{credential.name} token")
      StaticSecret.create!(
        namespace: credential.namespace,
        name: name,
        broker_credential: credential,
        inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" }
      )
    end

    test "updates the credential and wrapper secret names from Slack profile details" do
      Oauth::EnrichCredentialIdentityJob.slack_api_http = ->(url:, access_token:, params:) {
        assert_equal "AT", access_token
        if url == Oauth::EnrichCredentialIdentityJob::AUTH_TEST_ENDPOINT
          { "ok" => true, "user" => "fallback" }
        else
          assert_equal Oauth::EnrichCredentialIdentityJob::USERS_INFO_ENDPOINT, url
          assert_equal({ "user" => "U12345" }, params)
          {
            "ok" => true,
            "user" => {
              "profile" => {
                "display_name" => "Grace Hopper",
                "email" => "grace@example.com"
              }
            }
          }
        end
      }
      credential = slack_credential
      secret = wrap_credential(credential)

      Oauth::EnrichCredentialIdentityJob.perform_now(credential.id)

      assert_equal "Slack – Grace Hopper", credential.reload.name
      assert_equal "grace@example.com", credential.provider_email
      assert_equal "Slack – Grace Hopper token", secret.reload.name
    end

    test "uses auth test username without users read scope" do
      calls = []
      Oauth::EnrichCredentialIdentityJob.slack_api_http = ->(url:, access_token:, params:) {
        calls << [ url, params ]
        { "ok" => true, "user" => "grace" }
      }
      credential = slack_credential(scopes: %w[channels:history])
      secret = wrap_credential(credential)

      Oauth::EnrichCredentialIdentityJob.perform_now(credential.id)

      assert_equal "Slack – grace", credential.reload.name
      assert_equal "Slack – grace token", secret.reload.name
      assert_equal [ [ Oauth::EnrichCredentialIdentityJob::AUTH_TEST_ENDPOINT, {} ] ], calls
    end

    test "does not clobber an operator-renamed wrapper secret" do
      Oauth::EnrichCredentialIdentityJob.slack_api_http = ->(url:, access_token:, params:) {
        { "ok" => true, "user" => "grace" }
      }
      credential = slack_credential(scopes: %w[channels:history])
      secret = wrap_credential(credential, name: "operator name")

      Oauth::EnrichCredentialIdentityJob.perform_now(credential.id)

      assert_equal "Slack – grace", credential.reload.name
      assert_equal "operator name", secret.reload.name
    end
  end
end
