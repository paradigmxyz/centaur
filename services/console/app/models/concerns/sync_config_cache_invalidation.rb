module SyncConfigCacheInvalidation
  extend ActiveSupport::Concern

  included do
    before_save :mark_sync_config_cache_invalidation_for_save
    before_destroy :mark_sync_config_cache_invalidation
    before_commit :bump_sync_config_cache_for_record, if: :sync_config_cache_invalidation_needed?
    after_commit :clear_sync_config_cache_invalidation
    after_rollback :clear_sync_config_cache_invalidation
  end

  private

  def mark_sync_config_cache_invalidation_for_save
    changes = changes_to_save.except("created_at", "updated_at")
    mark_sync_config_cache_invalidation if new_record? || changes.present?
  end

  def mark_sync_config_cache_invalidation
    @sync_config_cache_invalidation_needed = true
  end

  def sync_config_cache_invalidation_needed?
    @sync_config_cache_invalidation_needed == true
  end

  def clear_sync_config_cache_invalidation
    remove_instance_variable(:@sync_config_cache_invalidation_needed) if
      instance_variable_defined?(:@sync_config_cache_invalidation_needed)
  end

  def bump_sync_config_cache_for_record
    Principal.bump_sync_config_cache_versions(sync_config_affected_principals)
  end

  # Grantable secret models intentionally use broad invalidation. Admin writes to
  # these rows are rare compared with proxy sync polling, and over-invalidation is
  # safer than maintaining a per-model list of config-affecting columns.
  def sync_config_affected_principals
    Principal.effective_grantees_for_grantable(self)
  end
end
