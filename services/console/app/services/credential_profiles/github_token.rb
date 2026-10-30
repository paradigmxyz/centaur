module CredentialProfiles
  # GitHub API clients authenticate with Bearer/token while Git-over-HTTPS uses
  # HTTP Basic. Replacing a placeholder preserves the scheme selected by each
  # client and confines the credential to GitHub's API, Git, and hosted MCP
  # server hosts.
  module GithubToken
    KIND = "github_token"
    REPLACE_CONFIG = {
      "proxy_value" => "GITHUB_TOKEN",
      "match_headers" => [ "Authorization" ],
      "require" => false
    }.freeze
    RULE_ATTRIBUTES = [
      { host: "api.github.com", http_methods: [], paths: [], position: 0 },
      { host: "github.com", http_methods: [], paths: [], position: 1 },
      { host: "api.githubcopilot.com", http_methods: [], paths: [], position: 2 }
    ].freeze
    ALLOWED_HOSTS = RULE_ATTRIBUTES.map { |attributes| attributes[:host] }.freeze

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

    # Host-based rather than an exact match against RULE_ATTRIBUTES so secrets
    # seeded before a host was added stay valid on later saves.
    def validate_rules(secret, rules:)
      actual = Array(rules)
      confined = actual.present? && actual.all? do |rule|
        ALLOWED_HOSTS.include?(rule.host) &&
          rule.cidr.blank? && rule.http_methods.blank? && rule.paths.blank?
      end
      return if confined

      secret.errors.add(
        :rules,
        "github_token credentials must target only #{ALLOWED_HOSTS.join(', ')}"
      )
    end

    def canonical_replace_config?(config)
      config.blank? || config == REPLACE_CONFIG || config == REPLACE_CONFIG.except("require")
    end
  end
end
