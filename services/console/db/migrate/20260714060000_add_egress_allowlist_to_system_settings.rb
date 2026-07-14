class AddEgressAllowlistToSystemSettings < ActiveRecord::Migration[8.0]
  def change
    # Off by default: an existing deployment upgrades with its egress behavior
    # byte-for-byte unchanged. Turning this on is a deliberate operator act,
    # because it is a hard cutover -- see Principal#egress_rules.
    add_column :system_settings, :egress_allowlist_mode, :string,
      default: "off", null: false

    # The hosts a sandbox must reach regardless of which credentials it holds:
    # the model provider, package registries, the git host. Derivation alone
    # cannot supply these -- they carry no secret, so no secret's rules name
    # them -- and an allowlist without them is a sandbox that cannot boot.
    add_column :system_settings, :egress_allowlist_base_rules, :jsonb,
      default: [], null: false
  end
end
