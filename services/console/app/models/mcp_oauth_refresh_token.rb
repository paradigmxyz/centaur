class McpOauthRefreshToken < ApplicationRecord
  include HashedTokenLookup

  oid_prefix "mor"
  token_hash_attribute :token_hash

  DEFAULT_TTL = 90.days
  TOKEN_PREFIX = "mcprt_".freeze

  attr_accessor :plaintext_token

  belongs_to :mcp_oauth_client
  belongs_to :user
  belongs_to :principal

  before_validation :issue_token, on: :create

  validates :token_hash, presence: true, uniqueness: true
  validates :resource, :expires_at, presence: true
  validates :scopes, presence: true

  scope :usable, -> { where(revoked_at: nil).where("expires_at > ?", Time.current) }

  def revoke!
    update!(revoked_at: Time.current)
  end

  private

  def issue_token
    self.expires_at ||= DEFAULT_TTL.from_now
    return if token_hash.present?
    self.plaintext_token = "#{TOKEN_PREFIX}#{SecureRandom.urlsafe_base64(48)}"
    self.token_hash = self.class.hash_token(plaintext_token)
  end
end
