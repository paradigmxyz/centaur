require "test_helper"
require Rails.root.join("db/migrate/20260806190200_backfill_github_token_kind")

class BackfillGithubTokenKindTest < ActiveSupport::TestCase
  def create_github_secret!(name:, inject_config: nil, replace_config: nil)
    secret = StaticSecret.create!(
      name: name,
      inject_config: inject_config,
      replace_config: replace_config,
      created_by: users(:acme_admin)
    )
    RequestRule.create!(host: "api.github.com", position: 0, static_secret: secret)
    RequestRule.create!(host: "github.com", position: 1, static_secret: secret)
    secret
  end

  test "backfills canonical GitHub Bearer credentials" do
    secret = create_github_secret!(
      name: "legacy GitHub token",
      inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" }
    )
    BackfillGithubTokenKind.new.up

    secret.reload
    assert_equal "github_token", secret.kind
    assert_nil secret.inject_config
    assert_equal CredentialProfiles::GithubToken::REPLACE_CONFIG, secret.replace_config
  end

  test "marks an already repaired canonical GitHub credential with its kind" do
    secret = create_github_secret!(
      name: "repaired GitHub token",
      replace_config: CredentialProfiles::GithubToken::REPLACE_CONFIG
    )

    BackfillGithubTokenKind.new.up

    assert_equal "github_token", secret.reload.kind
    assert_equal CredentialProfiles::GithubToken::REPLACE_CONFIG, secret.replace_config
  end

  test "leaves API-only, custom-format, and expanded GitHub credentials custom" do
    api_only = StaticSecret.create!(
      name: "API token",
      inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
      created_by: users(:acme_admin)
    )
    RequestRule.create!(host: "api.github.com", static_secret: api_only)

    custom = StaticSecret.create!(
      name: "custom GitHub header",
      inject_config: { "header" => "Authorization", "formatter" => "token {{ .Value }}" },
      created_by: users(:acme_admin)
    )
    RequestRule.create!(host: "api.github.com", position: 0, static_secret: custom)
    RequestRule.create!(host: "github.com", position: 1, static_secret: custom)

    expanded = create_github_secret!(
      name: "expanded GitHub token",
      inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" }
    )
    RequestRule.create!(host: "uploads.github.com", position: 2, static_secret: expanded)

    BackfillGithubTokenKind.new.up

    assert_equal "custom", api_only.reload.kind
    assert_equal "custom", custom.reload.kind
    assert_equal "custom", expanded.reload.kind
  end
end
