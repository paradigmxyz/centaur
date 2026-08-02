class PrincipalIdentifier < ApplicationRecord
  belongs_to :principal, inverse_of: :principal_identifiers

  attr_readonly :principal_id, :namespace, :scheme, :issuer, :subject

  normalizes :namespace, :scheme, :issuer, :subject, with: ->(value) { value.to_s.strip }

  validates :namespace, :scheme, :subject, presence: true
  validates :subject, uniqueness: { scope: %i[principal_id scheme issuer] }
  validate :namespace_matches_principal

  before_validation :copy_principal_namespace
  before_validation :normalize_metadata

  def api_payload
    {
      scheme: scheme,
      issuer: issuer,
      subject: subject,
      metadata: metadata.to_h
    }
  end

  private

  def copy_principal_namespace
    self.namespace = principal.namespace if new_record? && principal
  end

  def normalize_metadata
    self.metadata = metadata.to_h
  end

  def namespace_matches_principal
    return unless principal && namespace.present?
    return if namespace == principal.namespace

    errors.add(:namespace, "must match the principal namespace")
  end
end
