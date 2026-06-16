# Request-rule params, shared by the secret kinds that support rules (every kind
# except pg_dsn). The hidden http_methods/paths inputs arrive comma-joined; rows
# that are entirely blank are dropped, and position is assigned from form order.
# The owning controller calls #assign_rules from its #assign_form.
module RuleParams
  extend ActiveSupport::Concern

  private

  def assign_rules(secret)
    rows = (params[:rules]&.to_unsafe_h || {})
           .sort_by { |index, _| index.to_i }
           .map { |_, row| row }
           .reject { |r| r.values_at("host", "cidr", "http_methods", "paths").all?(&:blank?) }

    secret.rules = rows.each_with_index.map do |r, position|
      RequestRule.new(
        host: r["host"].presence,
        cidr: r["cidr"].presence,
        http_methods: split_terms(r["http_methods"]).map(&:upcase),
        paths: split_terms(r["paths"]),
        position: position
      )
    end
  end

  def split_terms(value)
    value.to_s.split(/[\s,]+/).map(&:strip).reject(&:blank?)
  end
end
