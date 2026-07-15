require "test_helper"

class GithubRequesterIdentityTest < ActiveSupport::TestCase
  setup do
    GithubRequesterIdentity.github_api_http = nil
  end

  teardown do
    GithubRequesterIdentity.github_api_http = nil
  end

  test "resolves the login stored on the Console user's connected GitHub account" do
    credential = github_credential(labels: { "github_login" => "goksu" })
    GithubRequesterIdentity.github_api_http = ->(**) { flunk("GitHub API should not be called") }

    result = GithubRequesterIdentity.resolve(user: credential.created_by)

    assert_equal "@goksu", result.handle
    assert_equal "connected GitHub account", result.source
  end

  test "resolves older connected credentials through the GitHub API" do
    credential = github_credential(labels: {})
    GithubRequesterIdentity.github_api_http = ->(url:, access_token:) {
      assert_equal GithubRequesterIdentity::USER_ENDPOINT, url
      assert_equal "gho-requester", access_token
      { "login" => "goksu" }
    }

    result = GithubRequesterIdentity.resolve(user: credential.created_by)

    assert_equal "@goksu", result.handle
    assert_equal "connected GitHub account (GitHub API)", result.source
  end

  test "does not adopt another user's connected GitHub account" do
    github_credential(created_by: users(:acme_admin), labels: { "github_login" => "someone-else" })

    result = GithubRequesterIdentity.resolve(user: users(:member_user))

    assert_nil result.handle
    assert_equal "no connected GitHub account found", result.reason
  end

  private

  def github_credential(created_by: users(:member_user), labels:)
    app = oauth_apps(:acme_github)
    app.update!(client_secret: "github-secret")
    BrokerCredential.create!(
      namespace: app.credential_namespace,
      oauth_app: app,
      created_by: created_by,
      provider_subject: "12345",
      provider_email: created_by.email,
      labels: labels,
      token_endpoint: app.provider_strategy.token_endpoint,
      access_token: "gho-requester",
      scopes: %w[repo]
    )
  end
end
