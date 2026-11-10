# A private SSH key that iron-proxy can present to a deployment-allowed SSH
# gateway on a sandbox's behalf. It is granted like every other secret: a
# principal or role grant decides who may use it. Unlike the other kinds it is
# never part of the synced proxy config; proxy-sync releases it one connection
# at a time through the proxy's SSH authorizer.
#
# The private key is one owned secret source. An inline (control_plane) key is
# parsed on save to derive and verify the public key; other source types resolve
# on the proxy host, so their public key must be supplied.
#
# A personal key has an owner and is managed from the owner's Integrations page.
# PersonalSshKeyGrants grants it to the owner's own principals; sharing it with
# anything else is an explicit admin grant.
class SshKeySecret < ApplicationRecord
  oid_prefix "sks"

  include ForeignIdCollisionGuard

  URL_SAFE_FORMAT = /\A[A-Za-z0-9\-._~]+\z/
  URL_SAFE_MESSAGE = "must contain only URL-safe characters (A-Z, a-z, 0-9, -, ., _, ~)"

  has_one :key_source, class_name: "SecretSource", dependent: :destroy, autosave: true
  has_many :grants, dependent: :destroy
  belongs_to :owner, class_name: "User", optional: true
  belongs_to :created_by, class_name: "User"

  normalizes :name, with: ->(value) { value.to_s.strip }
  # Keep only "<type> <base64>": comments are labels, not key material.
  normalizes :public_key, with: ->(value) { value.to_s.split.first(2).join(" ") }

  before_validation :derive_inline_public_key
  after_create_commit :grant_to_owner_principals, if: :owner_id?

  validates :name, presence: true, length: { maximum: 100 }
  validates :foreign_id, uniqueness: { allow_nil: true },
            format: { with: URL_SAFE_FORMAT, message: URL_SAFE_MESSAGE }, allow_nil: true
  validates :owner_id, uniqueness: { allow_nil: true }
  validate :labels_is_a_hash
  validate :private_key_present_and_valid
  validate :public_key_supported

  # Stores +value+ as this key's inline private key, encrypted at rest. The
  # public key is derived (and the value checked) when the record validates.
  def private_key=(value)
    if key_source&.source_type == "control_plane"
      key_source.secret = value
    else
      self.key_source = SecretSource.new(source_type: "control_plane", secret: value)
    end
  end

  def personal? = owner_id.present?

  def key_fingerprint
    SshKey.fingerprint(public_key)
  rescue SshKey::Invalid
    nil
  end

  private

  # Runs on every validation, not only when the source looks dirty: replacing
  # a persisted record's has_one saves the new source immediately, so by
  # validation time it is neither new nor changed. Deriving each time keeps the
  # stored public key consistent with whatever inline key is actually stored.
  def derive_inline_public_key
    @private_key_error = nil
    source = key_source
    return unless source&.source_type == "control_plane" && !source.marked_for_destruction?

    normalized = SshKey.normalize(source.secret) if source.secret.present?
    source.secret = normalized if normalized && normalized != source.secret
    self.public_key = SshKey.public_key(source.secret.to_s)
  rescue SshKey::Invalid => error
    # SshKey messages never include the submitted key.
    @private_key_error = error.message
  end

  def private_key_present_and_valid
    if key_source.nil? || key_source.marked_for_destruction?
      errors.add(:private_key, "can't be blank")
    elsif key_source.source_type == "token_broker"
      errors.add(:private_key, "can't come from a token broker")
    elsif @private_key_error
      errors.add(:private_key, @private_key_error)
    end
  end

  def public_key_supported
    return if @private_key_error

    type, body = public_key.to_s.split
    unless SshKey::SUPPORTED_TYPES.include?(type) && body.to_s.match?(%r{\A[A-Za-z0-9+/]+={0,2}\z})
      errors.add(:public_key, "must be an Ed25519, RSA, or ECDSA public key in OpenSSH format")
    end
  end

  def labels_is_a_hash
    errors.add(:labels, "must be a hash") unless labels.is_a?(Hash)
  end

  def grant_to_owner_principals
    PersonalSshKeyGrants.new.apply_for_user(owner)
  end
end
