require "test_helper"
require Rails.root.join("db/migrate/20260806190000_replace_github_bearer_injection")

class ReplaceGithubBearerInjectionTest < ActiveSupport::TestCase
  test "converts GitHub host Bearer injection and invalidates granted principal snapshots" do
    secret = StaticSecret.create!(
      name: "legacy GitHub token",
      inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
      created_by: users(:acme_admin)
    )
    RequestRule.create!(host: "github.com", static_secret: secret)
    principal = principals(:acme_channel)
    Grant.create!(principal: principal, static_secret: secret, created_by: users(:acme_admin))
    previous_version = principal.reload.sync_config_cache_version

    ReplaceGithubBearerInjection.new.up

    secret.reload
    assert_nil secret.inject_config
    assert_equal CredentialProfiles::GithubToken::REPLACE_CONFIG, secret.replace_config
    assert_equal previous_version + 1, principal.reload.sync_config_cache_version
  end

  test "leaves API-only and non-Bearer GitHub credentials unchanged" do
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
    RequestRule.create!(host: "github.com", static_secret: custom)

    ReplaceGithubBearerInjection.new.up

    assert api_only.reload.inject_config.present?
    assert_nil api_only.replace_config
    assert custom.reload.inject_config.present?
    assert_nil custom.replace_config
  end
end
