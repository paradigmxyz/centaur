# Finds OAuth-flow credentials (Slack/Google/GitHub/...) that appear to belong
# to the same human as an existing user principal, then automatically grants
# their wrapper static secrets to that principal. Matching precedence per
# provider: the principal's provider-native identity, else the union of
# matches through the credential owner's Slack SSO identity and email.
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
  SLACK_PROVIDER = Oauth::Providers::Slack::KEY
  GOOGLE_PROVIDER = Oauth::Providers::Google::KEY
  EMAIL_LABELS = %w[email google_email].freeze
  # Ordinary principal labels carrying a provider-native identity. Slack uses
  # first-class columns instead. When a principal has a native identity, it
  # takes precedence over email matching for that provider's credentials.
  # Providers without an entry (for example github) match through the
  # credential owner's Slack SSO identity or by email.
  PROVIDER_SUBJECT_LABELS = {
    GOOGLE_PROVIDER => %w[google_subject]
  }.freeze
  SLACK_TEAM_LABEL = "slack_team_id"

  def entries
    indexes = credential_indexes
    user_principals.select { |principal| user_principal?(principal) }.filter_map do |principal|
      entry_for(principal, indexes: indexes)
    end.sort_by do |entry|
      [ entry.principal.name.to_s, entry.principal.foreign_id.to_s ]
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
    user_principals.find_each do |principal|
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
    if console_user_principal?(principal)
      sync_console_user_slack_identity(principal, credentials)
      return
    end

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
        owner_index: indexes[provider][:owners],
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
  # single-principal path with a candidate query
  # (`LOWER(provider_email) IN (...) OR provider_subject IN (...)`, backed by
  # indexes on LOWER(provider_email) and provider_subject), which is O(K) in the credentials of the one matched
  # human. Keep the SQL normalization identical to normalize_email /
  # normalize_key. entries/apply_all legitimately need the full load.
  def credential_indexes
    providers.index_with do |provider|
      credentials = provider_credentials(provider)
      prime_owner_slack_identities(credentials)
      {
        subjects: index_by_subject(credentials),
        emails: index_by_email(credentials),
        owners: index_by_owner_identity(credentials)
      }
    end
  end

  def provider_credentials(provider)
    BrokerCredential
      .joins(:oauth_app)
      .includes(:oauth_app, :static_secret)
      .where(oauth_apps: { provider: provider })
      .order(:id)
      .to_a
  end

  def user_principals
    Principal.order(:id)
  end

  def user_principal?(principal)
    labels = principal.labels || {}
    return true if [ USER_KIND, CONSOLE_USER_KIND ].include?(principal.kind)
    return true if principal.slack_user_id.present? || principal.slack_email.present?

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

  def index_by_owner_identity(credentials)
    credentials.each_with_object(Hash.new { |hash, key| hash[key] = [] }) do |credential, acc|
      identity = owner_slack_identity_for(credential)
      acc[identity] << credential if identity
    end
  end

  def provider_credentials_for(principal, provider:, subject_index:, email_index:, owner_index:, emails:)
    native = credentials_for_subject_labels(principal, provider, subject_index)
    return native if native.any?

    (credentials_for_owner_identity(principal, provider, owner_index) +
      credentials_for_emails(principal, emails, email_index, provider)).uniq
  end

  def credentials_for_subject_labels(principal, provider, subject_index)
    return [] if console_user_principal?(principal)

    subjects = principal_subjects(principal, provider)
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

  def credentials_for_owner_identity(principal, provider, owner_index)
    identity = principal_slack_identity(principal)
    return [] unless identity

    (owner_index[identity] || [])
      .select { |credential| credential_matches_principal?(principal, credential, provider) }
  end

  def credential_matches_principal?(principal, credential, provider = nil)
    provider ||= credential.oauth_app&.provider
    return false unless supported_provider?(credential)
    return false if provider == SLACK_PROVIDER && !slack_team_matches?(principal, credential)
    if console_user_principal?(principal)
      return principal_emails(principal).include?(normalize_email(credential.provider_email))
    end

    subjects = principal_subjects(principal, provider)
    if subjects.any?
      subjects.include?(normalize_key(credential.provider_subject))
    else
      owner_identity_matches_principal?(principal, credential) ||
        principal_emails(principal).include?(normalize_email(credential.provider_email))
    end
  end

  # A credential also matches through the console user who consented to it:
  # when that user's single Slack SSO identity
  # (UserIdentity.unambiguous_slack_identity) equals the principal's Slack
  # identity. This is what lets providers whose credentials carry no usable
  # identity (github: provider_email is the usually-empty public profile
  # email) reconcile without manual grants. The owner side is authenticated --
  # created_by is set server-side at consent, the identity comes from Slack's
  # OIDC id_token -- while the principal's slack_user_id/slack_team_id carry
  # the same operator-write trust as the email labels the email fallback uses.
  def owner_identity_matches_principal?(principal, credential)
    identity = principal_slack_identity(principal)
    return false unless identity

    owner_slack_identity_for(credential) == identity
  end

  def principal_slack_identity(principal)
    return nil if console_user_principal?(principal)

    subject = normalize_key(principal.slack_user_id)
    team = normalize_key(principal.slack_team_id)
    [ subject, team ] if subject && team
  end

  def owner_slack_identity_for(credential)
    user_id = credential.created_by_id
    return nil if user_id.blank?

    owner_slack_identities.fetch(user_id) do
      owner_slack_identities[user_id] = unambiguous_slack_identity(
        UserIdentity.slack.where(user_id: user_id)
      )
    end
  end

  def prime_owner_slack_identities(credentials)
    missing = credentials.filter_map(&:created_by_id).uniq - owner_slack_identities.keys
    return if missing.empty?

    identities_by_user = UserIdentity.slack.where(user_id: missing).group_by(&:user_id)
    missing.each do |user_id|
      owner_slack_identities[user_id] = unambiguous_slack_identity(identities_by_user[user_id] || [])
    end
  end

  def unambiguous_slack_identity(identities)
    UserIdentity.unambiguous_slack_identity(identities)&.map { |value| normalize_key(value) }
  end

  def owner_slack_identities
    @owner_slack_identities ||= {}
  end

  def subject_label_keys(provider)
    PROVIDER_SUBJECT_LABELS.fetch(provider, [])
  end

  def principal_subjects(principal, provider)
    if provider == SLACK_PROVIDER
      return [ normalize_key(principal.slack_user_id) ].compact
    end

    subject_label_keys(provider)
      .filter_map { |key| normalize_key(principal.labels&.[](key)) }
      .uniq
  end

  def supported_provider?(credential)
    providers.include?(credential.oauth_app&.provider)
  end

  # Slack user ids are workspace-scoped. If either side carries a team identity,
  # require both sides to agree; otherwise global identity matching is the available
  # boundary for older credentials.
  def slack_team_matches?(principal, credential)
    principal_team = normalize_key(principal.slack_team_id)
    credential_team = normalize_key(credential.labels&.[](SLACK_TEAM_LABEL)) ||
                      normalize_key(credential.oauth_app&.labels&.[](SLACK_TEAM_LABEL))
    return true if console_user_principal?(principal) && principal_team.blank?
    return true if principal_team.blank? && credential_team.blank?

    principal_team.present? && principal_team == credential_team
  end

  def sync_console_user_slack_identity(principal, credentials)
    slack_credentials = credentials.select do |credential|
      credential.oauth_app&.provider == SLACK_PROVIDER
    end
    return if slack_credentials.empty?

    slack_user_id = unique_present_value(slack_credentials.map(&:provider_subject))
    slack_team_id = unique_present_value(slack_credentials.map { |credential| slack_team_for(credential) })
    return unless slack_user_id && slack_team_id
    return unless Principal::SLACK_USER_ID_FORMAT.match?(slack_user_id)
    return unless Principal::SLACK_TEAM_ID_FORMAT.match?(slack_team_id)

    updates = {
      slack_user_id: slack_user_id,
      slack_team_id: slack_team_id
    }
    return if updates.all? { |field, value| principal.public_send(field) == value }

    principal.update!(updates)
  end

  def slack_team_for(credential)
    credential.labels&.[](SLACK_TEAM_LABEL).presence ||
      credential.oauth_app&.labels&.[](SLACK_TEAM_LABEL).presence
  end

  def console_user_principal?(principal)
    principal.kind == CONSOLE_USER_KIND
  end

  def principal_emails(principal)
    if console_user_principal?(principal)
      return console_user_emails(principal).filter_map { |email| normalize_email(email) }.uniq
    end

    labels = principal.labels || {}
    (EMAIL_LABELS.map { |key| labels[key] } + [ principal.slack_email ])
      .filter_map { |email| normalize_email(email) }
      .uniq
  end

  # Console-user principals reference the console user's database row, so every verified
  # identity email of that user participates in matching -- a credential
  # registered under a secondary verified email still reaches the principal.
  # Unverified emails are excluded: an unverified address must not adopt
  # someone else's credentials.
  def console_user_emails(principal)
    user_id = principal.console_user_id
    return [] if user_id.blank?

    @console_user_emails ||= {}
    @console_user_emails.fetch(user_id) do
      user = User.find_by(id: user_id)
      emails = if user
        [ user.email ] + user.user_identities.where(email_verified: true).pluck(:email)
      else
        []
      end
      @console_user_emails[user_id] = emails
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
