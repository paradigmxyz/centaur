module Console
  # Create/edit form for GcpAuthSecret: mints short-lived GCP OAuth2 access tokens
  # from either a service-account keyfile source or workload-identity (metadata
  # server) credentials, scoped to one or more OAuth scopes, with optional
  # domain-wide delegation (subject, keyfile only) and request rules.
  class GcpAuthSecretsController < BaseSecretsController
    include RuleParams

    private

    def model
      GcpAuthSecret
    end

    def kind
      "gcp_auth"
    end

    # keyfile and credentials_provider are mutually exclusive (enforced by the
    # model). The form's mode radio picks one; the other is cleared so a stale
    # value from a prior save can't linger. subject is keyfile-only.
    def assign_form(secret)
      assign_identity(secret)
      gcp = params.fetch(:gcp, ActionController::Parameters.new)
      secret.scopes = split_terms(gcp[:scopes])

      if gcp[:credential_mode] == "workload_identity"
        secret.keyfile_source = nil
        secret.credentials_provider = { "type" => "workload_identity" }
        secret.subject = nil
      else
        secret.credentials_provider = nil
        secret.keyfile_source = build_source
        secret.subject = gcp[:subject].presence
      end

      assign_rules(secret)
    end
  end
end
