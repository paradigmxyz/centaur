require "test_helper"
require Rails.root.join("db/migrate/20260806190000_replace_github_bearer_injection")

class ReplaceGithubBearerInjectionTest < ActiveSupport::TestCase
  test "converts GitHub host Bearer injection and invalidates granted principal snapshots" do
    secret = StaticSecret.create!(
      namespace: "acme",
      name: "legacy GitHub token",
      inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
      created_by: users(:acme_admin)
    )
    rule = RequestRule.create!(host: "github.invalid", static_secret: secret)
    rule.update_column(:host, "github.com")
    principal = principals(:acme_channel)
    Grant.create!(principal: principal, static_secret: secret, created_by: users(:acme_admin))
    previous_version = principal.reload.sync_config_cache_version

    ReplaceGithubBearerInjection.new.up

    secret.reload
    assert_nil secret.inject_config
    assert_equal(
      {
        "proxy_value" => "GITHUB_TOKEN",
        "match_headers" => [ "Authorization" ],
        "require" => false
      },
      secret.replace_config
    )
    assert_equal previous_version + 1, principal.reload.sync_config_cache_version
  end

  test "leaves API-only and non-Bearer GitHub credentials unchanged" do
    api_only = StaticSecret.create!(
      namespace: "acme",
      name: "API token",
      inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" },
      created_by: users(:acme_admin)
    )
    RequestRule.create!(host: "api.github.com", static_secret: api_only)
    custom = StaticSecret.create!(
      namespace: "acme",
      name: "custom GitHub header",
      inject_config: { "header" => "Authorization", "formatter" => "token {{ .Value }}" },
      created_by: users(:acme_admin)
    )
    rule = RequestRule.create!(host: "github.invalid", static_secret: custom)
    rule.update_column(:host, "github.com")

    ReplaceGithubBearerInjection.new.up

    assert api_only.reload.inject_config.present?
    assert_nil api_only.replace_config
    assert custom.reload.inject_config.present?
    assert_nil custom.replace_config
  end
end
