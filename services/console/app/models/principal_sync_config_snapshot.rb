class PrincipalSyncConfigSnapshot < ApplicationRecord
  TTL = 10.minutes
  RETENTION = 1.hour

  belongs_to :principal

  encrypts :payload
  serialize :payload, coder: JSON

  validates :principal_cache_version, presence: true
  validates :principal_id, uniqueness: { scope: :principal_cache_version }

  # Returns the freshest usable snapshot, stale-while-revalidate style. When
  # the current-version snapshot is stale or missing, exactly one caller
  # rebuilds it (non-blocking row lock on the principal); concurrent callers
  # are served the stale snapshot immediately instead of queuing behind the
  # rebuild. Config invalidations fan out to every proxy of a principal at
  # once (cache-version bumps, TTL expiry), so blocking here previously
  # stampeded all of them onto one row lock, each holding a request thread
  # and DB connection for the full rebuild.
  #
  # Serving a stale snapshot is safe: iron-proxy treats the config hash as an
  # ETag and re-applies on its next 5s poll once the rebuild lands. Only a
  # cold start (no snapshot at any version) blocks until the build finishes,
  # because there is nothing stale to serve.
  def self.fetch_for(principal)
    version = principal.sync_config_cache_version
    snapshot = find_by(principal: principal, principal_cache_version: version)
    return snapshot if snapshot&.fresh_for?(principal)

    try_build_for(principal) || snapshot || latest_for(principal) || build_for(principal)
  end

  def self.prune_expired!
    where("updated_at < ?", RETENTION.ago).delete_all
  end

  def fresh?
    updated_at >= TTL.ago
  end

  def fresh_for?(principal)
    fresh? && !api_server_jwt_window_stale?(principal)
  end

  # Most recent snapshot at any cache version; the stale fallback while
  # another session rebuilds. Old versions survive until prune_expired!
  # (RETENTION), which comfortably covers a rebuild.
  def self.latest_for(principal)
    where(principal: principal).order(updated_at: :desc).first
  end

  def self.build_for(principal)
    principal.with_lock { build_within_lock(principal) }
  rescue ActiveRecord::RecordNotUnique
    retry
  end

  # Non-blocking variant of build_for: acquires the principal row lock with
  # SKIP LOCKED and returns nil when another session already holds it.
  def self.try_build_for(principal)
    transaction do
      locked = Principal.lock("FOR UPDATE SKIP LOCKED").find_by(id: principal.id)
      next nil unless locked

      build_within_lock(locked)
    end
  rescue ActiveRecord::RecordNotUnique
    retry
  end

  # Assumes the caller holds the principal's row lock and passes the freshly
  # locked (reloaded) record, so sync_config_cache_version is current.
  def self.build_within_lock(principal)
    version = principal.sync_config_cache_version
    snapshot = find_or_initialize_by(principal: principal, principal_cache_version: version)
    return snapshot if snapshot.persisted? && snapshot.fresh_for?(principal)

    snapshot.payload = principal.effective_config(redact_secrets: false)
    if snapshot.changed?
      snapshot.save!
    else
      # A rebuild that yields an identical payload must still restart the TTL,
      # or the snapshot stays permanently stale and every poll re-runs the
      # expensive effective_config rebuild.
      snapshot.touch
    end
    snapshot
  end

  def api_server_jwt_window_stale?(principal)
    return false unless principal.sandbox_api_server_enabled?

    return false if principal.slack_jwt_channel_ids.empty?
    return false if ENV["CENTAUR_JWT_SIGNING_SECRET"].to_s.blank?

    updated_at.to_i < ApiServer::Jwt.window_start_for(principal, Time.current.to_i)
  end
end
