class User < ApplicationRecord
  oid_prefix "usr"

  # validations: false because SSO-only users have no password. The password
  # length rule below still applies to anyone who does set one (password login is
  # kept as a break-glass fallback).
  has_secure_password validations: false

  has_many :api_keys, dependent: :destroy
  has_many :user_identities, dependent: :destroy
  belongs_to :approved_by, class_name: "User", optional: true

  # pending: signed in via SSO but not yet approved -- cannot use the console.
  # active: approved operator. disabled: access revoked.
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

  # Resolves the console user behind a verified SSO identity, creating or linking
  # as needed, and (re)caches the identity's email/name. A returning login matches
  # by the stable (provider, subject). A new identity links to an existing user
  # only when the IdP-verified email matches -- an unverified email must never
  # adopt an account -- otherwise a new user is created: active + admin when the
  # email is on the bootstrap allowlist, pending otherwise. +identity+ is the
  # provider strategy's { subject:, email:, email_verified:, name: } hash.
  def self.link_or_provision(provider:, identity:)
    transaction do
      if (existing = UserIdentity.find_by(provider: provider, subject: identity[:subject]))
        existing.update!(email: identity[:email], email_verified: identity[:email_verified])
        user = existing.user
        user.update!(name: identity[:name]) if identity[:name].present? && user.name.blank?
        next user
      end

      user = linkable_user(identity) || create!(provisioned_attributes(identity))
      user.user_identities.create!(
        provider: provider, subject: identity[:subject],
        email: identity[:email], email_verified: identity[:email_verified]
      )
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

  # Attributes for a brand-new SSO user: active + admin when bootstrap-allowlisted
  # by a verified IdP email, pending otherwise.
  def self.provisioned_attributes(identity)
    admin = identity[:email_verified] == true && ConsoleAuth.bootstrap_admin?(identity[:email])
    { email: identity[:email], name: identity[:name], status: admin ? :active : :pending, admin: admin }
  end
  private_class_method :provisioned_attributes
end
