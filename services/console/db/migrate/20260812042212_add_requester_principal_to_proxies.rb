class AddRequesterPrincipalToProxies < ActiveRecord::Migration[8.1]
  def change
    # The proxy outlives its requester: deleting the requester principal just
    # unbinds it, exactly like the conversation principal FK.
    add_reference :proxies, :requester_principal,
                  foreign_key: { to_table: :principals, on_delete: :nullify }
    add_column :proxies, :requester_principal_assigned_at, :datetime
  end
end
