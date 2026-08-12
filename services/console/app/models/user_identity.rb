# A linked SSO identity for a console User: the provider plus the IdP's stable
# subject. A returning sign-in is matched by (provider, subject), which never
# changes, rather than by email, which can. email/email_verified are cached from
# the last sign-in for display and to gate email-based account linking.
class UserIdentity < ApplicationRecord
  oid_prefix "usid"

  belongs_to :user

  PROVIDERS = %w[google slack].freeze
  SLACK_PROVIDER = "slack".freeze

  scope :slack, -> { where(provider: SLACK_PROVIDER) }

  # A Slack identity that arrives (or gains its team) after the user already
  # consented to OAuth credentials is the one ordering the Principal/
  # BrokerCredential/StaticSecret hooks cannot observe; re-run reconciliation
  # over the owner's credentials so the grant still materializes. The update
  # trigger is scoped to the fields that participate in matching, so the email
  # re-cache on every returning SSO login does not re-run reconciliation.
  after_commit :reconcile_owner_oauth_credentials,
               on: %i[create update],
               if: -> {
                 slack? && (previously_new_record? || saved_change_to_subject? ||
                   saved_change_to_team_id? || saved_change_to_user_id?)
               }

  normalizes :email, with: ->(e) { e.to_s.strip.downcase.presence }
  normalizes :team_id, with: ->(id) { id.to_s.strip.presence }

  validates :provider, presence: true, inclusion: { in: PROVIDERS }
  validates :subject, presence: true, uniqueness: { scope: :provider }

  def slack? = provider == SLACK_PROVIDER

  # The single native Slack identity in ``identities``, as ``[subject,
  # team_id]``, or nil. Slack's OIDC id_token is the authenticated source of
  # these values; an ambiguous account (several distinct pairs) is refused
  # rather than guessed at, and the chosen pair must be well-formed Slack ids.
  def self.unambiguous_slack_identity(identities)
    pairs = identities.filter_map do |identity|
      next if identity.subject.blank? || identity.team_id.blank?

      [ identity.subject, identity.team_id ]
    end.uniq
    return nil unless pairs.one?

    subject, team_id = pairs.first
    return nil unless Principal::SLACK_USER_ID_FORMAT.match?(subject)
    return nil unless Principal::SLACK_TEAM_ID_FORMAT.match?(team_id)

    pairs.first
  end

  private

  # Never let reconciliation abort the surrounding write: this hook runs inside
  # the SSO login's commit path, and a failure here must not break sign-in.
  def reconcile_owner_oauth_credentials
    reconciliation = PrincipalCredentialReconciliation.new
    BrokerCredential.joins(:oauth_app).where(created_by_id: user_id)
      .select(:id).find_each do |credential|
      reconciliation.apply_for_credential(credential)
    end
  rescue StandardError => e
    Rails.logger.warn("user_identity_credential_reconciliation_failed error=#{e.class}")
  end
end
