module CredentialProfiles
  # GitHub API clients authenticate with Bearer/token while Git-over-HTTPS uses
  # HTTP Basic. Replacing a placeholder preserves the scheme selected by each
  # client and confines the credential to GitHub's API and Git hosts.
  module GithubToken
    KIND = "github_token"
    REPLACE_CONFIG = {
      "proxy_value" => "GITHUB_TOKEN",
      "match_headers" => [ "Authorization" ],
      "require" => false
    }.freeze
    RULE_ATTRIBUTES = [
      { host: "api.github.com", http_methods: [], paths: [], position: 0 },
      { host: "github.com", http_methods: [], paths: [], position: 1 }
    ].freeze

    module_function

    def apply_defaults(secret, rules:)
      if secret.inject_config.blank? && canonical_replace_config?(secret.replace_config)
        secret.replace_config = REPLACE_CONFIG.deep_dup
      end
      rules.presence || RULE_ATTRIBUTES.map { |attributes| RequestRule.new(attributes) }
    end

    def validate_config(secret)
      return if secret.inject_config.blank? && secret.replace_config == REPLACE_CONFIG

      secret.errors.add(
        :base,
        "github_token credentials must use the canonical Authorization placeholder replacement"
      )
    end

    def validate_rules(secret, rules:)
      actual = Array(rules).map do |rule|
        {
          host: rule.host,
          cidr: rule.cidr,
          http_methods: rule.http_methods,
          paths: rule.paths,
          position: rule.position
        }.compact
      end
      return if actual == RULE_ATTRIBUTES

      secret.errors.add(
        :rules,
        "github_token credentials must target only api.github.com and github.com"
      )
    end

    def canonical_replace_config?(config)
      config.blank? || config == REPLACE_CONFIG || config == REPLACE_CONFIG.except("require")
    end
  end
end
