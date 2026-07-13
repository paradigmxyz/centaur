class User < ApplicationRecord
  oid_prefix "usr"

  # validations: false because SSO-only users have no password. The password
  # length rule below still applies to anyone who does set one (password login is
  # kept as a break-glass fallback).
  has_secure_password validations: false

  has_many :api_keys, dependent: :destroy
  has_many :mcp_oauth_refresh_tokens, dependent: :destroy
  has_many :user_identities, dependent: :destroy
  belongs_to :approved_by, class_name: "User", optional: true

  after_update :revoke_mcp_oauth_refresh_tokens_when_disabled,
               if: -> { saved_change_to_status? && disabled? }

  # active: normal operator (SSO users are provisioned active). pending: legacy
  # state from the retired approval queue, flipped to active on next SSO login.
  # disabled: access revoked.
  enum :status, { pending: "pending", active: "active", disabled: "disabled" },
       default: :pending, validate: true

  normalizes :email, with: ->(e) { e.strip.downcase }

  validates :email,
            presence: true,
            uniqueness: true,
            format: { with: URI::MailTo::EMAIL_REGEXP }
  validates :password, length: { minimum: 12 }, allow_nil: true

  # Marks a pending user active, recording who approved them and when.
  def approve!(by:)
    update!(status: :active, approved_at: Time.current, approved_by: by)
  end

  def revoke_mcp_oauth_refresh_tokens!
    now = Time.current
    mcp_oauth_refresh_tokens.usable.update_all(revoked_at: now, updated_at: now)
  end

  # Resolves the console user behind a verified SSO identity, creating or linking
  # as needed, and (re)caches the identity's email/name. A returning login matches
  # by the stable (provider, subject). A new identity links to an existing user
  # only when the IdP-verified email matches -- an unverified email must never
  # adopt an account -- otherwise a new active user is created (admin when the
  # verified email is on the bootstrap allowlist). +identity+ is the provider
  # strategy's { subject:, email:, email_verified:, name: } hash.
  def self.link_or_provision(provider:, identity:)
    transaction do
      user =
        if (existing = UserIdentity.find_by(provider: provider, subject: identity[:subject]))
          existing.update!(identity_attributes(provider:, identity:))
          existing.user.tap do |u|
            u.update!(name: identity[:name]) if identity[:name].present? && u.name.blank?
          end
        else
          (linkable_user(identity) || create!(provisioned_attributes(identity))).tap do |u|
            u.user_identities.create!(
              identity_attributes(provider:, identity:).merge(provider:, subject: identity[:subject])
            )
          end
        end
      activate_on_login(user)
      user
    end
  end

  # An existing user this identity may attach to: only when the IdP marked the
  # email verified (an unverified email must not adopt an existing account).
  def self.linkable_user(identity)
    return nil unless identity[:email_verified] && identity[:email].present?
    find_by(email: identity[:email].strip.downcase)
  end
  private_class_method :linkable_user

  def self.identity_attributes(provider:, identity:)
    attributes = { email: identity[:email], email_verified: identity[:email_verified] }
    if provider == UserIdentity::SLACK_PROVIDER && identity[:team_id].present?
      attributes[:team_id] = identity[:team_id]
    end
    attributes
  end
  private_class_method :identity_attributes

  # Attributes for a brand-new SSO user: everyone is provisioned active -- the
  # console is only reachable on the internal network, so a completed SSO login
  # is sufficient and there is no admin-approval queue. Admin additionally
  # requires a bootstrap-allowlisted, IdP-verified email.
  def self.provisioned_attributes(identity)
    admin = identity[:email_verified] == true && ConsoleAuth.bootstrap_admin?(identity[:email])
    { email: identity[:email], name: identity[:name], status: :active, admin: admin }
  end
  private_class_method :provisioned_attributes

  # Flips a pending user to active on login: covers accounts provisioned pending
  # under the old approval-queue policy. Never touches disabled accounts and
  # never grants admin.
  def self.activate_on_login(user)
    return unless user.pending?
    user.update!(status: :active, approved_at: Time.current)
  end
  private_class_method :activate_on_login

  private

  def revoke_mcp_oauth_refresh_tokens_when_disabled
    revoke_mcp_oauth_refresh_tokens!
  end
end
