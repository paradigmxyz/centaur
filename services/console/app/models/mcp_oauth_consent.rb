require "digest"

# The server-side, single-use half of an MCP consent ticket
# (Mcp::ConsentTicket). Holds only a digest of the ticket nonce.
class McpOauthConsent < ApplicationRecord
  belongs_to :user

  scope :pending, -> { where(consumed_at: nil).where("expires_at > ?", Time.current) }

  def self.digest(nonce) = Digest::SHA256.hexdigest(nonce.to_s)

  def self.pending_for?(nonce, user)
    pending.exists?(nonce_digest: digest(nonce), user_id: user.id)
  end

  # Atomically marks the consent decided. Exactly one caller wins, however
  # many requests race or replay: the conditional UPDATE matches the row only
  # while it is still unconsumed and unexpired.
  def self.consume!(nonce, user)
    now = Time.current
    pending.where(nonce_digest: digest(nonce), user_id: user.id)
           .update_all(consumed_at: now, updated_at: now) == 1
  end

  # Rows past their expiry are useless; keep the table small.
  def self.prune!
    where("expires_at < ?", 1.day.ago).delete_all
  end
end
