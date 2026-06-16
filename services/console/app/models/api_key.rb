class ApiKey < ApplicationRecord
  oid_prefix "ak"

  TOKEN_PREFIX = "iak_".freeze
  TOKEN_FORMAT = /\Aiak_[0-9a-f]{64}\z/

  attr_readonly :user_id, :token_hash
  attr_accessor :token

  belongs_to :user

  default_scope { where(deleted_at: nil) }

  validates :name, presence: true
  validates :token_hash, presence: true, uniqueness: true
  validate :token_matches_format, on: :create

  before_validation :issue_token, on: :create

  def self.find_by_token(plaintext)
    return nil if plaintext.blank?
    find_by(token_hash: hash_token(plaintext))
  end

  def self.hash_token(plaintext)
    Digest::SHA256.hexdigest(plaintext)
  end

  def soft_delete!(at: Time.current)
    update_column(:deleted_at, at)
  end

  def deleted?
    deleted_at.present?
  end

  private

  def issue_token
    return if token_hash.present?
    self.token = "#{TOKEN_PREFIX}#{SecureRandom.hex(32)}"
    self.token_hash = self.class.hash_token(token)
  end

  def token_matches_format
    return if token.blank?
    return if token.match?(TOKEN_FORMAT)
    errors.add(:token, "must match #{TOKEN_FORMAT.inspect} (iak_ + 32-byte lowercase hex)")
  end
end
