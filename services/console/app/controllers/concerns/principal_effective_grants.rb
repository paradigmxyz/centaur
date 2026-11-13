# GET .../principals/:id/effective_grants for the sandbox admin API and the
# console API: the secrets a principal holds directly or through roles, with
# how each one is reached. Paginated (page/limit) like other lists.
module PrincipalEffectiveGrants
  extend ActiveSupport::Concern

  def effective_grants
    principal = Principal.find_by_oid!(params[:id])
    limit = pagination_limit
    result = PrincipalEffectiveGrantsPage.new(
      principal: principal,
      relations: principal.granted_secrets_by_kind,
      page: pagination_page,
      per_page: limit
    ).call

    data = result.records_by_kind.flat_map do |kind, secrets|
      secrets.map do |secret|
        effective_grant_payload(kind, secret, result.sources_by_key[[ kind, secret.id ]])
      end
    end
    total_pages = result.total_count.zero? ? 0 : result.total_pages
    render json: { data: data, meta: { page: result.page, limit: limit,
                                       total: result.total_count, total_pages: total_pages } }
  end

  private

  def effective_grant_payload(kind, secret, sources)
    roles = sources.filter_map { |source| source[:role] }.uniq
    {
      id: secret.oid,
      grant_key: PrincipalEffectiveGrantsPage::GRANT_COLUMN_BY_KIND.fetch(kind),
      foreign_id: secret.foreign_id,
      name: secret.try(:name),
      direct: sources.any? { |source| source[:type] == :direct },
      roles: roles.map { |role| { id: role.oid, foreign_id: role.foreign_id, name: role.name } }
    }
  end
end
