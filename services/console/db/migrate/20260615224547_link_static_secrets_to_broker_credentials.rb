class LinkStaticSecretsToBrokerCredentials < ActiveRecord::Migration[8.1]
  def change
    # The OAuth consent flow auto-creates a static secret wrapping a minted broker
    # credential. The flow is unauthenticated, so that secret has no operator --
    # like the credential itself, whose created_by is already nullable.
    change_column_null :static_secrets, :created_by_id, true

    # First-class link from a wrapping static secret to the broker credential it
    # references (the token_broker source still carries the credential_id the
    # proxy resolves at sync; this association is the console-level link). Nullable
    # and nullify-on-delete: an ordinary static secret has no broker credential.
    # The index is unique among non-null values: at most one managed wrapper per
    # credential (a has_one), while ordinary secrets keep a null reference.
    add_reference :static_secrets, :broker_credential, null: true, foreign_key: true,
                  index: { unique: true, where: "broker_credential_id IS NOT NULL" }
  end
end
