class PrincipalEffectiveGrantsPage
  Result = Data.define(:records_by_kind, :sources_by_key, :page, :total_pages, :total_count)
  GRANT_COLUMN_BY_KIND = Grant::GRANTABLE_ASSOCIATIONS.to_h do |association|
    [ association.to_s.delete_suffix("_secret"), :"#{association}_id" ]
  end.freeze

  def initialize(principal:, relations:, page:, per_page:)
    @principal = principal
    @relations = relations
    @requested_page = page
    @per_page = per_page
  end

  def call
    counts = effective_secret_counts
    total_count = counts.values.sum
    total_pages = [ (total_count.to_f / per_page).ceil, 1 ].max
    page = [ normalized_page, total_pages ].min
    records_by_kind = page_records(counts, offset: (page - 1) * per_page)

    Result.new(
      records_by_kind: records_by_kind,
      sources_by_key: sources_for(records_by_kind),
      page: page,
      total_pages: total_pages,
      total_count: total_count
    )
  end

  private

  attr_reader :principal, :relations, :requested_page, :per_page

  def normalized_page
    page = Integer(requested_page.to_s, 10, exception: false) || 1
    page < 1 ? 1 : page
  end

  def effective_secret_counts
    kinds = relations.keys
    columns = kinds.map { |kind| GRANT_COLUMN_BY_KIND.fetch(kind) }
    aggregates = columns.map { |column| Grant.arel_table[column].count(true) }
    counts = principal.effective_grants.pick(*aggregates)

    kinds.zip(Array(counts)).to_h
  end

  def page_records(counts, offset:)
    remaining = per_page
    records = relations.to_h { |kind, _relation| [ kind, [] ] }

    relations.each do |kind, relation|
      count = counts.fetch(kind)
      if offset >= count
        offset -= count
        next
      end

      limit = [ remaining, count - offset ].min
      records[kind] = relation.offset(offset).limit(limit).to_a
      remaining -= limit
      break if remaining.zero?

      offset = 0
    end

    records
  end

  # How each displayed effective secret is reached, keyed by [kind, secret_id].
  # A value contains { type: :direct } and/or { type: :role, role: } entries
  # because the same secret can be granted directly and through several roles.
  # Limit this attribution query to the current page rather than loading every
  # effective grant merely to render one bounded table page.
  def sources_for(records_by_kind)
    selected = records_by_kind.filter_map do |kind, records|
      ids = records.map(&:id)
      [ kind, ids ] if ids.any?
    end
    return {} if selected.empty?

    selected_scope = selected.reduce(Grant.none) do |scope, (kind, ids)|
      scope.or(Grant.where(GRANT_COLUMN_BY_KIND.fetch(kind) => ids))
    end
    grants = principal.effective_grants.merge(selected_scope).includes(:role)

    grants.each_with_object(Hash.new { |hash, key| hash[key] = [] }) do |grant, sources|
      association = Grant::GRANTABLE_ASSOCIATIONS.find { |name| grant.public_send("#{name}_id") }
      next unless association

      kind = association.to_s.delete_suffix("_secret")
      key = [ kind, grant.public_send("#{association}_id") ]
      sources[key] << (grant.role ? { type: :role, role: grant.role } : { type: :direct })
    end
  end
end
