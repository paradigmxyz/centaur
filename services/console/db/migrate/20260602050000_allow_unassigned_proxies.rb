class AllowUnassignedProxies < ActiveRecord::Migration[8.1]
  def change
    # A proxy can now boot before it has a principal: principal_id becomes
    # optional and principal_assigned_at records when the current assignment was
    # made (null while unassigned) so the proxy can detect a swap on sync.
    change_column_null :proxies, :principal_id, true
    add_column :proxies, :principal_assigned_at, :datetime

    up_only do
      execute "UPDATE proxies SET principal_assigned_at = created_at WHERE principal_id IS NOT NULL"
    end

    # A proxy now outlives its principal: deleting a principal leaves the proxy
    # unassigned (principal_id nulled) rather than destroying the proxy.
    remove_foreign_key :proxies, :principals
    add_foreign_key :proxies, :principals, on_delete: :nullify
  end
end
