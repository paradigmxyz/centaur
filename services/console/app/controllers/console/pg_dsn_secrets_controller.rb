module Console
  # Create/edit form for PgDsnSecret: a required database routing key, optional
  # upstream role, optional pinned session settings, and a single DSN source
  # (no rules).
  class PgDsnSecretsController < BaseSecretsController
    private

    def model
      PgDsnSecret
    end

    def kind
      "pg_dsn"
    end

    def assign_form(secret)
      assign_identity(secret)
      pg = params.fetch(:secret, ActionController::Parameters.new)
      secret.database = pg[:database].presence
      secret.role = pg[:role].presence
      secret.settings = setting_rows(params[:settings])
      secret.dsn_source = build_source
    end

    # The pinned session settings as an ordered array of { "name", "value" } or
    # { "name", "value_from" } hashes (order matters: the proxy applies them in
    # sequence). Each row's kind select picks a literal value or a principal
    # reference. Rows with a blank name are dropped; the model validates names,
    # uniqueness, and references.
    def setting_rows(raw)
      (raw&.to_unsafe_h || {}).values.filter_map do |row|
        name = row["name"].to_s.strip
        next if name.blank?
        kind = row["kind"].to_s
        if PgDsnSecret::VALUE_FROM_KEYS.include?(kind)
          { "name" => name, "value_from" => { kind => row["value"].to_s.strip } }
        else
          { "name" => name, "value" => row["value"].to_s }
        end
      end
    end
  end
end
