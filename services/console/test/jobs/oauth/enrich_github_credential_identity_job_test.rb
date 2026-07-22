require "test_helper"

module Oauth
  class EnrichGithubCredentialIdentityJobTest < ActiveJob::TestCase
    setup do
      Oauth::EnrichGithubCredentialIdentityJob.github_api_http = nil
    end

    teardown do
      Oauth::EnrichGithubCredentialIdentityJob.github_api_http = nil
    end

    def github_credential(**overrides)
      app = oauth_apps(:acme_github)
      app.update!(client_secret: "github-secret")
      BrokerCredential.create!({
        namespace: "acme",
        foreign_id: "github-github-pending-abc123",
        name: "GitHub – Pending GitHub account",
        token_endpoint: Oauth::Providers::Github::TOKEN_ENDPOINT,
        oauth_app: app,
        provider_subject: "pending-abc123",
        access_token: "gho-token",
        refresh_token: nil,
        scopes: %w[repo read:user]
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

    test "updates the credential and wrapper secret names from GitHub profile details" do
      Oauth::EnrichGithubCredentialIdentityJob.github_api_http = ->(url:, access_token:) {
        assert_equal Oauth::EnrichGithubCredentialIdentityJob::USER_ENDPOINT, url
        assert_equal "gho-token", access_token
        { "id" => 99_123, "login" => "octocat", "name" => "Octo Cat", "email" => "octo@example.com" }
      }
      credential = github_credential
      secret = wrap_credential(credential)

      Oauth::EnrichGithubCredentialIdentityJob.perform_now(credential.id)

      assert_equal "GitHub – Octo Cat", credential.reload.name
      assert_equal "99123", credential.provider_subject
      assert_equal "octo@example.com", credential.provider_email
      assert_equal "github-github-99123", credential.foreign_id
      assert_equal "octocat", credential.labels["github_login"]
      assert_equal "GitHub – Octo Cat token", secret.reload.name
    end

    test "falls back to login for display name" do
      Oauth::EnrichGithubCredentialIdentityJob.github_api_http = ->(url:, access_token:) {
        { "id" => 99_123, "login" => "octocat", "name" => nil, "email" => nil }
      }
      credential = github_credential
      secret = wrap_credential(credential)

      Oauth::EnrichGithubCredentialIdentityJob.perform_now(credential.id)

      assert_equal "GitHub – octocat", credential.reload.name
      assert_equal "GitHub – octocat token", secret.reload.name
    end

    test "does not clobber an operator-renamed wrapper secret" do
      Oauth::EnrichGithubCredentialIdentityJob.github_api_http = ->(url:, access_token:) {
        { "id" => 99_123, "login" => "octocat", "name" => "Octo Cat" }
      }
      credential = github_credential
      secret = wrap_credential(credential, name: "operator name")

      Oauth::EnrichGithubCredentialIdentityJob.perform_now(credential.id)

      assert_equal "GitHub – Octo Cat", credential.reload.name
      assert_equal "operator name", secret.reload.name
    end
  end
end
