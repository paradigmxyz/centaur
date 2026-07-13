# A linked SSO identity for a console User: the provider plus the IdP's stable
# subject. A returning sign-in is matched by (provider, subject), which never
# changes, rather than by email, which can. email/email_verified are cached from
# the last sign-in for display and to gate email-based account linking.
class UserIdentity < ApplicationRecord
  oid_prefix "usid"

  belongs_to :user

  PROVIDERS = %w[google slack].freeze
  SLACK_PROVIDER = "slack".freeze

  normalizes :email, with: ->(e) { e.to_s.strip.downcase.presence }
  normalizes :team_id, with: ->(id) { id.to_s.strip.presence }

  validates :provider, presence: true, inclusion: { in: PROVIDERS }
  validates :subject, presence: true, uniqueness: { scope: :provider }
end
