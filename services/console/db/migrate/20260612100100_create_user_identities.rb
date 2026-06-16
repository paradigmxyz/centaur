class CreateUserIdentities < ActiveRecord::Migration[8.1]
  # A linked SSO identity for a console user. One user can have several (e.g. both
  # Google and Slack); a returning user is matched by the stable (provider,
  # subject) pair rather than by their mutable email.
  def change
    create_table :user_identities do |t|
      t.references :user, null: false, foreign_key: true
      t.string :provider, null: false
      t.string :subject, null: false
      t.string :email
      t.boolean :email_verified, null: false, default: false

      t.timestamps
    end
    add_index :user_identities, [ :provider, :subject ], unique: true
  end
end
