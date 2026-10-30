class SshConnection < ApplicationRecord
  belongs_to :user
  encrypts :private_key

  normalizes :name, with: ->(value) { value.to_s.strip }
  before_validation :prepare_connection
  validates :user_id, uniqueness: true
  validates :name, presence: true, length: { maximum: 100 }
  validates :generation, presence: true
  validate :valid_private_key, unless: :revoked?

  def configured? = !revoked? && (expires_at.nil? || expires_at.future?)
  def revoked? = revoked_at.present?

  def revoke!
    update!(revoked_at: Time.current, private_key: nil)
  end

  def key_fingerprint
    SshKey.fingerprint(public_key)
  rescue SshKey::Invalid
    nil
  end

  private

  def prepare_connection
    self.generation = SecureRandom.uuid if new_record? || changed?
  end

  def valid_private_key
    return if persisted? && !will_save_change_to_private_key? && private_key.present?

    self.public_key = SshKey.public_key(private_key)
    self.private_key = SshKey.normalize(private_key)
  rescue SshKey::Invalid => error
    errors.add(:private_key, error.message)
  end
end
