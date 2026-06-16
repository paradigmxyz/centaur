module SyncConfigOwnerInvalidation
  extend ActiveSupport::Concern

  include SyncConfigCacheInvalidation

  def sync_config_owner
    self.class::OWNER_ASSOCIATIONS.each do |assoc|
      id = public_send("#{assoc}_id")
      next if id.blank?

      return assoc.to_s.classify.constantize.find_by(id: id)
    end
    nil
  end

  private

  def sync_config_affected_principal_ids
    owner = sync_config_owner
    owner ? Principal.effective_grantee_ids_for_grantable(owner) : []
  end
end
