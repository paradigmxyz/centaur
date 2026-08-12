require "test_helper"

module CredentialProfiles
  class GithubTokenTest < ActiveSupport::TestCase
    test "applies canonical configuration and explicit request rules" do
      secret = StaticSecret.new(kind: "github_token")

      rules = secret.apply_kind_defaults(rules: [])
      secret.rules = rules

      assert_nil secret.inject_config
      assert_equal GithubToken::REPLACE_CONFIG, secret.replace_config
      assert_equal GithubToken::RULE_ATTRIBUTES, rules.map { |rule|
        rule.attributes.symbolize_keys.slice(:host, :http_methods, :paths, :position)
      }
      assert secret.valid?, secret.errors.full_messages.inspect
    end

    test "does not overwrite conflicting caller configuration or rules" do
      secret = StaticSecret.new(
        kind: "github_token",
        inject_config: { "header" => "Authorization", "formatter" => "Bearer {{ .Value }}" }
      )
      supplied_rules = [ RequestRule.new(host: "example.com") ]

      rules = secret.apply_kind_defaults(rules: supplied_rules)
      secret.rules = rules

      assert_same supplied_rules, rules
      assert_not secret.valid?
      assert_includes secret.errors[:base],
                      "github_token credentials must use the canonical Authorization placeholder replacement"
      assert_includes secret.errors[:rules],
                      "github_token credentials must target only api.github.com and github.com"
    end

    test "normalizes an omitted false require flag" do
      secret = StaticSecret.new(
        kind: "github_token",
        replace_config: GithubToken::REPLACE_CONFIG.except("require")
      )

      secret.apply_kind_defaults(rules: [])

      assert_equal GithubToken::REPLACE_CONFIG, secret.replace_config
    end

    test "custom secrets are not changed by the registry" do
      secret = StaticSecret.new(inject_config: { "header" => "X-Api-Key" })
      rules = [ RequestRule.new(host: "example.com") ]

      assert_same rules, secret.apply_kind_defaults(rules: rules)
      assert_equal({ "header" => "X-Api-Key" }, secret.inject_config)
    end
  end
end
