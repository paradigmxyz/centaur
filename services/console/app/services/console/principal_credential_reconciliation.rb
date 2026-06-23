module Console
  # Finds OAuth-flow credentials that appear to belong to the same human as an
  # existing user principal, then grants their wrapper secrets on request.
  class PrincipalCredentialReconciliation
    Entry = Struct.new(
      :principal,
      :slack_credentials,
      :google_credentials,
      :slack_grants,
      :google_grants,
      keyword_init: true
    ) do
      def credentials
        slack_credentials + google_credentials
      end

      def actionable_credentials
        credentials.select { |credential| credential.static_secret && !granted?(credential) }
      end

      def granted?(credential)
        slack_grants[credential.id] || google_grants[credential.id] || false
      end
    end

    USER_KIND = "user"
    SLACK_PROVIDER = Oauth::Providers::Slack::KEY
    GOOGLE_PROVIDER = Oauth::Providers::Google::KEY
    EMAIL_LABELS = %w[email google_email slack_email].freeze
    SLACK_USER_LABELS = %w[slack_user_id].freeze
    GOOGLE_SUBJECT_LABELS = %w[google_subject].freeze
    SLACK_TEAM_LABEL = "slack_team_id"

    def entries
      slack = provider_credentials(SLACK_PROVIDER)
      google = provider_credentials(GOOGLE_PROVIDER)
      slack_by_subject = credentials_by_subject(slack)
      google_by_subject = credentials_by_subject(google)
      slack_by_email = credentials_by_email(slack)
      google_by_email = credentials_by_email(google)

      user_principals.filter_map do |principal|
        emails = principal_emails(principal)
        slack_credentials = provider_credentials_for(
          principal,
          subject_label_keys: SLACK_USER_LABELS,
          credentials_by_subject: slack_by_subject,
          credentials_by_email: slack_by_email,
          emails: emails,
          provider: SLACK_PROVIDER
        )
        google_credentials = provider_credentials_for(
          principal,
          subject_label_keys: GOOGLE_SUBJECT_LABELS,
          credentials_by_subject: google_by_subject,
          credentials_by_email: google_by_email,
          emails: emails,
          provider: GOOGLE_PROVIDER
        )

        next if slack_credentials.empty? && google_credentials.empty?

        Entry.new(
          principal: principal,
          slack_credentials: slack_credentials,
          google_credentials: google_credentials,
          slack_grants: grant_status(principal, slack_credentials),
          google_grants: grant_status(principal, google_credentials)
        )
      end.sort_by do |entry|
        [ entry.principal.namespace, entry.principal.name.to_s, entry.principal.foreign_id.to_s ]
      end
    end

    def apply(principal_oid:, credential_oids:, current_user:)
      principal = Principal.find_by_oid!(principal_oid)
      proposed = entries.find { |entry| entry.principal.id == principal.id }
      requested_oids = Array(credential_oids).reject(&:blank?)
      allowed = Array(proposed&.credentials).select(&:static_secret).index_by(&:oid)
      credentials = requested_oids.map do |oid|
        allowed.fetch(oid) do
          raise ActiveRecord::RecordNotFound,
                "credential #{oid} is not a reconciliation candidate"
        end
      end

      created = 0
      credentials.each do |credential|
        grant = principal.grants
          .create_with(created_by: current_user)
          .find_or_create_by!(static_secret: credential.static_secret)
        created += 1 if grant.previously_new_record?
      end

      { requested: credentials.size, created: created }
    end

    private

    def provider_credentials(provider)
      BrokerCredential
        .joins(:oauth_app)
        .includes(:oauth_app, :static_secret)
        .where(oauth_apps: { provider: provider })
        .order(:namespace, :id)
        .to_a
    end

    def user_principals
      Principal.order(:namespace, :id).select { |principal| user_principal?(principal) }
    end

    def user_principal?(principal)
      labels = principal.labels || {}
      labels["kind"] == USER_KIND ||
        (EMAIL_LABELS + SLACK_USER_LABELS + GOOGLE_SUBJECT_LABELS).any? do |key|
          labels[key].present?
        end
    end

    def credentials_by_subject(credentials)
      credentials.each_with_object(Hash.new { |hash, key| hash[key] = [] }) do |credential, acc|
        subject = normalize_key(credential.provider_subject)
        acc[subject] << credential if subject
      end
    end

    def credentials_by_email(credentials)
      credentials.each_with_object(Hash.new { |hash, key| hash[key] = [] }) do |credential, acc|
        email = normalize_email(credential.provider_email)
        acc[email] << credential if email
      end
    end

    def provider_credentials_for(
      principal,
      subject_label_keys:,
      credentials_by_subject:,
      credentials_by_email:,
      emails:,
      provider:
    )
      native = credentials_for_subject_labels(
        principal,
        subject_label_keys,
        credentials_by_subject,
        provider
      )
      return native if native.any?

      credentials_for_emails(principal, emails, credentials_by_email, provider)
    end

    def credentials_for_subject_labels(principal, label_keys, credentials_by_subject, provider)
      labels = principal.labels || {}
      subjects = label_keys.filter_map { |key| normalize_key(labels[key]) }.uniq
      subjects
        .flat_map { |subject| credentials_by_subject[subject] || [] }
        .select { |credential| credential_matches_principal?(principal, credential, provider) }
        .uniq
    end

    def credentials_for_emails(principal, emails, credentials_by_email, provider)
      emails
        .flat_map { |email| credentials_by_email[email] || [] }
        .select { |credential| credential_matches_principal?(principal, credential, provider) }
        .uniq
    end

    def credential_matches_principal?(principal, credential, provider)
      return false unless credential.namespace == principal.namespace
      return true unless provider == SLACK_PROVIDER

      slack_team_matches?(principal, credential)
    end

    # Slack user ids are workspace-scoped. If either side carries a team label,
    # require both sides to agree; otherwise namespace scoping is the available
    # boundary for older credentials.
    def slack_team_matches?(principal, credential)
      principal_team = normalize_key(principal.labels&.[](SLACK_TEAM_LABEL))
      credential_team = normalize_key(credential.labels&.[](SLACK_TEAM_LABEL)) ||
                        normalize_key(credential.oauth_app&.labels&.[](SLACK_TEAM_LABEL))
      return true if principal_team.blank? && credential_team.blank?
      principal_team.present? && principal_team == credential_team
    end

    def principal_emails(principal)
      labels = principal.labels || {}
      EMAIL_LABELS.map { |key| labels[key] }
        .filter_map { |email| normalize_email(email) }
        .uniq
    end

    def grant_status(principal, credentials)
      secret_ids = credentials.filter_map { |credential| credential.static_secret&.id }
      granted_secret_ids = if secret_ids.empty?
        []
      else
        principal.grants.where(static_secret_id: secret_ids).pluck(:static_secret_id)
      end

      credentials.each_with_object({}) do |credential, acc|
        acc[credential.id] =
          credential.static_secret && granted_secret_ids.include?(credential.static_secret.id)
      end
    end

    def normalize_key(value)
      value.to_s.strip.downcase.presence
    end

    def normalize_email(value)
      value.to_s.strip.downcase.presence
    end
  end
end
