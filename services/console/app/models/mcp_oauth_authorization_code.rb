class McpOauthAuthorizationCode < ApplicationRecord
  include HashedTokenLookup

  oid_prefix "moa"
  token_hash_attribute :code_hash

  CODE_TTL = 10.minutes
  TOKEN_PREFIX = "mcpauth_".freeze

  attr_accessor :plaintext_code

  belongs_to :mcp_oauth_client
  belongs_to :user
  belongs_to :principal

  before_validation :issue_code, on: :create

  validates :code_hash, presence: true, uniqueness: true
  validates :redirect_uri, :code_challenge, :resource, :expires_at, presence: true
  validates :scopes, presence: true

  scope :usable, -> { where(consumed_at: nil).where("expires_at > ?", Time.current) }

  def consume!
    update!(consumed_at: Time.current)
  end

  private

  def issue_code
    self.expires_at ||= CODE_TTL.from_now
    return if code_hash.present?
    self.plaintext_code = "#{TOKEN_PREFIX}#{SecureRandom.urlsafe_base64(48)}"
    self.code_hash = self.class.hash_token(plaintext_code)
  end
end
