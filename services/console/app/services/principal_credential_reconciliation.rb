# Finds OAuth-flow credentials (Slack/Google/GitHub/...) that appear to belong
# to the same human as an existing user principal, then automatically grants
# their wrapper static secrets to that principal.
class PrincipalCredentialReconciliation
  Entry = Struct.new(
    :principal,
    :credentials_by_provider,
    :granted_by_credential_id,
    keyword_init: true
  ) do
    def credentials
      credentials_by_provider.values.flatten
    end

    def credentials_for(provider)
      credentials_by_provider[provider] || []
    end

    def actionable_credentials
      credentials.select { |credential| credential.static_secret && !granted?(credential) }
    end

    def granted?(credential)
      granted_by_credential_id[credential.id] || false
    end
  end

  USER_KIND = "user"
  # Minted by the MCP OAuth flow (Mcp::OauthController#principal_for_current_user)
  # for a console user connecting an MCP client. These principals match
  # credentials only through their console User record (primary email plus
  # verified identity emails) -- never through mutable principal labels or
  # provider-subject labels, which would widen the trust boundary beyond the
  # authenticated user.
  CONSOLE_USER_KIND = "console_user"
  CONSOLE_USER_ID_LABEL = "console-user-id"
  SLACK_PROVIDER = Oauth::Providers::Slack::KEY
  GOOGLE_PROVIDER = Oauth::Providers::Google::KEY
  EMAIL_LABELS = %w[email google_email slack_email].freeze
  # Principal labels carrying a provider-native identity. When a principal has
  # one for a provider, it takes precedence over email matching for that
  # provider's credentials. Providers without an entry (for example github)
  # match by email only.
  PROVIDER_SUBJECT_LABELS = {
    SLACK_PROVIDER => %w[slack_user_id],
    GOOGLE_PROVIDER => %w[google_subject]
  }.freeze
  SLACK_TEAM_LABEL = "slack_team_id"

  def entries
    indexes = credential_indexes
    user_principals.select { |principal| user_principal?(principal) }.filter_map do |principal|
      entry_for(principal, indexes: indexes)
    end.sort_by do |entry|
      [ entry.principal.namespace, entry.principal.name.to_s, entry.principal.foreign_id.to_s ]
    end
  end

  def apply_for_principal(principal)
    apply_entry(entry_for(principal))
  end

  def apply_for_credential(credential)
    credential = BrokerCredential.includes(:oauth_app, :static_secret).find(credential.id)
    unless credential.static_secret && supported_provider?(credential)
      return { requested: 0, created: 0 }
    end

    requested = 0
    created = 0
    user_principals.where(namespace: credential.namespace).find_each do |principal|
      next unless user_principal?(principal)
      next unless credential_matches_principal?(principal, credential)

      requested += 1
      created += 1 if grant_credential(principal, credential)
      sync_principal_provider_labels(principal, [ credential ])
    end
    { requested: requested, created: created }
  end

  def apply_all
    entries.each_with_object({ requested: 0, created: 0 }) do |entry, totals|
      result = apply_entry(entry)
      totals[:requested] += result[:requested]
      totals[:created] += result[:created]
    end
  end

  private

  # Every registered OAuth-flow provider participates: a provider without
  # subject labels still reconciles by email, so new registry entries get
  # matching for free.
  def providers
    Oauth::Providers.keys
  end

  def apply_entry(entry)
    return { requested: 0, created: 0 } unless entry

    requested = entry.actionable_credentials.size
    created = entry.actionable_credentials.count do |credential|
      grant_credential(entry.principal, credential)
    end
    sync_principal_provider_labels(entry.principal, entry.credentials)
    { requested: requested, created: created }
  end

  def sync_principal_provider_labels(principal, credentials)
    # Console-user principals never match by label, so stamping provider
    # identity labels on them would only create stale, unused inputs.
    return if console_user_principal?(principal)

    google_credentials = credentials.select do |credential|
      credential.oauth_app&.provider == GOOGLE_PROVIDER
    end
    return if google_credentials.empty?

    labels = principal.labels || {}
    updates = {}
    subject = unique_present_value(google_credentials.map(&:provider_subject))
    email = unique_present_value(google_credentials.map(&:provider_email))

    updates["google_subject"] = subject if subject && labels["google_subject"].blank?
    updates["google_email"] = email if email && labels["google_email"].blank?
    return if updates.empty?

    principal.update!(labels: labels.merge(updates))
  end

  def grant_credential(principal, credential)
    secret = credential.static_secret
    return false unless secret
    return false if principal.grants.exists?(static_secret: secret)

    principal.grants.create!(static_secret: secret, created_by: principal.created_by)
    true
  rescue ActiveRecord::RecordNotUnique
    false
  end

  def entry_for(principal, indexes: nil)
    return nil unless user_principal?(principal)

    indexes ||= credential_indexes
    emails = principal_emails(principal)
    credentials_by_provider = providers.each_with_object({}) do |provider, acc|
      matched = provider_credentials_for(
        principal,
        provider: provider,
        subject_index: indexes[provider][:subjects],
        email_index: indexes[provider][:emails],
        emails: emails
      )
      acc[provider] = matched if matched.any?
    end
    return nil if credentials_by_provider.empty?

    Entry.new(
      principal: principal,
      credentials_by_provider: credentials_by_provider,
      granted_by_credential_id: grant_status(principal, credentials_by_provider.values.flatten)
    )
  end

  # TODO(perf): this loads every oauth-flow credential in the system -- O(C)
  # rows per Principal create/update, since apply_for_principal runs in an
  # after_commit. Negligible while C is in the hundreds. Add the optimization
  # when oauth-flow credential count reaches the low thousands or principal
  # writes show up in latency traces, whichever comes first: replace the
  # single-principal path with a candidate query (namespace-scoped
  # `LOWER(provider_email) IN (...) OR provider_subject IN (...)`, backed by
  # indexes on (namespace, LOWER(provider_email)) and (namespace,
  # provider_subject)), which is O(K) in the credentials of the one matched
  # human. Keep the SQL normalization identical to normalize_email /
  # normalize_key. entries/apply_all legitimately need the full load.
  def credential_indexes
    providers.index_with do |provider|
      credentials = provider_credentials(provider)
      { subjects: index_by_subject(credentials), emails: index_by_email(credentials) }
    end
  end

  def provider_credentials(provider)
    BrokerCredential
      .joins(:oauth_app)
      .includes(:oauth_app, :static_secret)
      .where(oauth_apps: { provider: provider })
      .order(:namespace, :id)
      .to_a
  end

  def user_principals
    Principal.order(:namespace, :id)
  end

  def user_principal?(principal)
    labels = principal.labels || {}
    return true if [ USER_KIND, CONSOLE_USER_KIND ].include?(labels["kind"])

    (EMAIL_LABELS + PROVIDER_SUBJECT_LABELS.values.flatten).any? do |key|
      labels[key].present?
    end
  end

  def index_by_subject(credentials)
    credentials.each_with_object(Hash.new { |hash, key| hash[key] = [] }) do |credential, acc|
      subject = normalize_key(credential.provider_subject)
      acc[subject] << credential if subject
    end
  end

  def index_by_email(credentials)
    credentials.each_with_object(Hash.new { |hash, key| hash[key] = [] }) do |credential, acc|
      email = normalize_email(credential.provider_email)
      acc[email] << credential if email
    end
  end

  def provider_credentials_for(principal, provider:, subject_index:, email_index:, emails:)
    native = credentials_for_subject_labels(principal, provider, subject_index)
    return native if native.any?

    credentials_for_emails(principal, emails, email_index, provider)
  end

  def credentials_for_subject_labels(principal, provider, subject_index)
    return [] if console_user_principal?(principal)

    labels = principal.labels || {}
    subjects = subject_label_keys(provider).filter_map { |key| normalize_key(labels[key]) }.uniq
    subjects
      .flat_map { |subject| subject_index[subject] || [] }
      .select { |credential| credential_matches_principal?(principal, credential, provider) }
      .uniq
  end

  def credentials_for_emails(principal, emails, email_index, provider)
    emails
      .flat_map { |email| email_index[email] || [] }
      .select { |credential| credential_matches_principal?(principal, credential, provider) }
      .uniq
  end

  def credential_matches_principal?(principal, credential, provider = nil)
    provider ||= credential.oauth_app&.provider
    return false unless supported_provider?(credential)
    return false unless credential.namespace == principal.namespace
    return false if provider == SLACK_PROVIDER && !slack_team_matches?(principal, credential)
    if console_user_principal?(principal)
      return principal_emails(principal).include?(normalize_email(credential.provider_email))
    end

    subjects = subject_label_keys(provider)
      .filter_map { |key| normalize_key(principal.labels&.[](key)) }
      .uniq
    if subjects.any?
      subjects.include?(normalize_key(credential.provider_subject))
    else
      principal_emails(principal).include?(normalize_email(credential.provider_email))
    end
  end

  def subject_label_keys(provider)
    PROVIDER_SUBJECT_LABELS.fetch(provider, [])
  end

  def supported_provider?(credential)
    providers.include?(credential.oauth_app&.provider)
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

  def console_user_principal?(principal)
    (principal.labels || {})["kind"] == CONSOLE_USER_KIND
  end

  def principal_emails(principal)
    if console_user_principal?(principal)
      return console_user_emails(principal).filter_map { |email| normalize_email(email) }.uniq
    end

    labels = principal.labels || {}
    EMAIL_LABELS.map { |key| labels[key] }
      .filter_map { |email| normalize_email(email) }
      .uniq
  end

  # Console-user principals carry the console user's oid, so every verified
  # identity email of that user participates in matching -- a credential
  # registered under a secondary verified email still reaches the principal.
  # Unverified emails are excluded: an unverified address must not adopt
  # someone else's credentials.
  def console_user_emails(principal)
    user_oid = principal.labels&.[](CONSOLE_USER_ID_LABEL)
    return [] if user_oid.blank?

    @console_user_emails ||= {}
    @console_user_emails.fetch(user_oid) do
      user = User.find_by_oid(user_oid)
      emails = if user
        [ user.email ] + user.user_identities.where(email_verified: true).pluck(:email)
      else
        []
      end
      @console_user_emails[user_oid] = emails
    end
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

  def unique_present_value(values)
    present = values.filter_map do |value|
      stripped = value.to_s.strip
      stripped.presence
    end.uniq { |value| value.downcase }

    present.one? ? present.first : nil
  end
end
