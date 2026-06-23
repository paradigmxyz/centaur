module Console
  # Create/edit form for GcpIdTokenSecret: mints a Google-signed OIDC ID token
  # from a service-account keyfile and injects it into Authorization, or
  # X-Serverless-Authorization for Cloud Run apps that need Authorization for
  # their own application auth.
  class GcpIdTokenSecretsController < BaseSecretsController
    include RuleParams

    private

    def model
      GcpIdTokenSecret
    end

    def kind
      "gcp_id_token"
    end

    def assign_form(secret)
      assign_identity(secret)
      gcp = params.fetch(:gcp_id_token, ActionController::Parameters.new)
      secret.audience = gcp[:audience].to_s.strip.presence
      secret.header = gcp[:header].to_s.strip.presence
      secret.keyfile_source = build_source
      assign_rules(secret)
    end
  end
end
