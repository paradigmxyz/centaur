module SyncConfigCacheInvalidation
  extend ActiveSupport::Concern

  included do
    before_commit :bump_sync_config_cache_for_record
  end

  private

  def bump_sync_config_cache_for_record
    Principal.bump_sync_config_cache_versions(sync_config_affected_principal_ids)
  end

  # Grantable secret models intentionally use broad invalidation. Admin writes to
  # these rows are rare compared with proxy sync polling, and over-invalidation is
  # safer than maintaining a per-model list of config-affecting columns.
  def sync_config_affected_principal_ids
    Principal.effective_grantee_ids_for_grantable(self)
  end
end
