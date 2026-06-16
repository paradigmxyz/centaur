module Console
  # Create/edit form for StaticSecret: an inject XOR replace config (enforced by
  # the model), one optional source, and any number of request rules.
  class StaticSecretsController < BaseSecretsController
    include RuleParams

    private

    def model
      StaticSecret
    end

    def kind
      "static"
    end

    def assign_form(secret)
      assign_identity(secret)
      st = params.fetch(:static, ActionController::Parameters.new)
      if st[:mode] == "replace"
        secret.inject_config = nil
        secret.replace_config = replace_config(st)
      else
        secret.replace_config = nil
        secret.inject_config = inject_config(st)
      end
      secret.source = build_source
      assign_rules(secret)
    end

    def inject_config(st)
      cfg = {}
      cfg["header"] = st[:header].strip if st[:header].present?
      cfg["query_param"] = st[:query_param].strip if st[:query_param].present?
      cfg["formatter"] = st[:formatter] if st[:formatter].present?
      cfg.presence
    end

    def replace_config(st)
      cfg = { "proxy_value" => st[:proxy_value].to_s }
      headers = st[:match_headers].to_s.split(",").map(&:strip).reject(&:blank?)
      cfg["match_headers"] = headers if headers.any?
      %w[match_body match_path match_query require].each do |flag|
        cfg[flag] = true if ActiveModel::Type::Boolean.new.cast(st[flag])
      end
      cfg
    end
  end
end
