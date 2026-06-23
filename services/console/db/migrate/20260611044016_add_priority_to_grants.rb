class AddPriorityToGrants < ActiveRecord::Migration[8.1]
  # Grant priority resolves collisions at the proxy: iron-proxy applies matching
  # transforms in array order and the last one wins, so the sync path emits
  # granted secrets in ascending priority order, making the highest-priority
  # grant authoritative. Direct (principal) grants outrank role grants by default;
  # the wide gap leaves room to interleave explicit priorities later. The values
  # are inlined rather than read from Grant so this migration stays self-contained.
  def up
    add_column :grants, :priority, :integer
    execute "UPDATE grants SET priority = 100 WHERE principal_id IS NOT NULL"
    execute "UPDATE grants SET priority = 0 WHERE role_id IS NOT NULL"
    change_column_null :grants, :priority, false
  end

  def down
    remove_column :grants, :priority
  end
end
