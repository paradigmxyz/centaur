class PrincipalSyncConfigSnapshot < ApplicationRecord
  TTL = 10.minutes
  RETENTION = 1.hour

  belongs_to :principal

  encrypts :payload
  serialize :payload, coder: JSON

  validates :principal_cache_version, presence: true
  validates :principal_id, uniqueness: { scope: :principal_cache_version }

  def self.fetch_for(principal)
    version = principal.sync_config_cache_version
    snapshot = find_by(principal: principal, principal_cache_version: version)
    return snapshot if snapshot&.fresh?

    build_for(principal)
  end

  def self.prune_expired!
    where("updated_at < ?", RETENTION.ago).delete_all
  end

  def fresh?
    updated_at >= TTL.ago
  end

  def self.build_for(principal)
    principal.with_lock do
      principal.reload
      version = principal.sync_config_cache_version
      snapshot = find_or_initialize_by(principal: principal, principal_cache_version: version)
      return snapshot if snapshot.persisted? && snapshot.fresh?

      config = principal.effective_config(redact_secrets: false)
      snapshot.payload = config
      snapshot.save!
      snapshot
    end
  rescue ActiveRecord::RecordNotUnique
    retry
  end
end
