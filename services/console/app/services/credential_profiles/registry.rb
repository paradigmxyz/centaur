module CredentialProfiles
  # Maps a persisted StaticSecret kind to the implementation that owns its
  # canonical proxy configuration and request rules. "custom" deliberately has
  # no profile: operators continue to configure those secrets directly.
  module Registry
    CUSTOM_KIND = "custom"
    PROFILES = {
      GithubToken::KIND => GithubToken
    }.freeze
    KINDS = [ CUSTOM_KIND, *PROFILES.keys ].freeze

    module_function

    def kinds
      KINDS
    end

    def apply_defaults(secret, rules:)
      profile = PROFILES[secret.kind]
      profile ? profile.apply_defaults(secret, rules: rules) : rules
    end

    def validate_config(secret)
      PROFILES[secret.kind]&.validate_config(secret)
    end

    def validate_rules(secret, rules:)
      PROFILES[secret.kind]&.validate_rules(secret, rules: rules)
    end
  end
end
