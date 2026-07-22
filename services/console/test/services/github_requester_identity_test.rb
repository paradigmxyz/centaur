require "test_helper"

class GithubRequesterIdentityTest < ActiveSupport::TestCase
  test "resolves the login stored on the Console user's connected GitHub account" do
    credential = github_credential(labels: { "github_login" => "goksu" })

    result = GithubRequesterIdentity.resolve(user: credential.created_by)

    assert_equal "@goksu", result.handle
    assert_equal "connected GitHub account", result.source
  end

  test "leaves older connected credentials for background enrichment" do
    credential = github_credential(labels: {})

    result = GithubRequesterIdentity.resolve(user: credential.created_by)

    assert_nil result.handle
    assert_equal "connected GitHub account is awaiting login enrichment", result.reason
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
