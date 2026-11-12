module Api
  module V1
    module Console
      # GET /api/v1/console/secrets
      #
      # Grantable secrets of every kind in one list (for grant pickers and the
      # console secrets table). Only identifying metadata is returned, plus the
      # source backend labels and the managing OAuth integration -- never source
      # references or values. Ordered by name (case-insensitive), then creation.
      #
      # Query params: q (name or foreign_id, case-insensitive), kind (one of the
      # secret kind slugs), exclude_grantee ("principal:<oid>" or "role:<oid>";
      # omits secrets granted directly to that grantee), page, limit.
      #
      # GET /api/v1/console/secrets/:kind/:id
      #
      # The console detail view of one secret of any kind: identity, the managing
      # integration, source backends and references (an inline value is shown as
      # "inline"), injection target, configuration rows, signed headers, and
      # request rules. Unknown kinds and ids are 404s.
      class SecretsController < BaseController
        def index
          scopes = filtered_scopes
          limit = pagination_limit
          page = pagination_page
          total = count_secrets(scopes)
          page_rows = page_of_secrets(scopes, limit: limit, offset: (page - 1) * limit)
          total_pages = total.zero? ? 0 : ((total + limit - 1) / limit)
          render json: {
            data: page_rows.map { |secret| secret_payload(secret) },
            meta: { page: page, limit: limit, total: total, total_pages: total_pages }
          }
        end

        def show
          secret = SecretKinds.find_secret!(params[:kind], params[:id])
          render json: { data: detail_payload(secret) }
        end

        private

        # The list's order: name (case-insensitive, unnamed first, compared
        # bytewise), then creation time and id, then kind as the final tiebreak.
        # Only one page of ids leaves the database; its records are loaded per
        # kind with the associations the payload reads.
        def page_of_secrets(scopes, limit:, offset:)
          return [] if offset.negative?

          secrets = secrets_union(scopes)
          query = Arel::SelectManager.new(secrets).project(secrets[:kind], secrets[:id])
                                     .order(Arel.sql('secrets.sort_name COLLATE "C"'), secrets[:created_at],
                                            secrets[:id], secrets[:kind_position])
                                     .take(limit).skip(offset)
          rows = ActiveRecord::Base.connection.select_rows(query)
          loaded = rows.group_by(&:first).to_h do |kind, kind_rows|
            config = SecretKinds::SECRET_KINDS.fetch(kind)
            scope = config.fetch(:model).includes(config.fetch(:includes))
            scope = scope.includes(broker_credential: :oauth_app) if config.fetch(:model) == StaticSecret
            [ kind, scope.where(id: kind_rows.map(&:second)).index_by(&:id) ]
          end
          rows.map { |kind, id| loaded.fetch(kind).fetch(id) }
        end

        def count_secrets(scopes)
          secrets = secrets_union(scopes)
          query = Arel::SelectManager.new(secrets).project(Arel.star.count)
          ActiveRecord::Base.connection.select_value(query).to_i
        end

        # Every selected kind's filtered rows as one derived table, "secrets",
        # with the columns the list sorts on.
        def secrets_union(scopes)
          selects = scopes.map.with_index do |(kind, scope), position|
            table = scope.arel_table
            scope.select(
              Arel::Nodes.build_quoted(kind).as("kind"),
              Arel::Nodes.build_quoted(position).as("kind_position"),
              table[:id].as("id"),
              Arel::Nodes::NamedFunction.new("lower", [
                Arel::Nodes::NamedFunction.new("coalesce", [ table[:name], Arel::Nodes.build_quoted("") ])
              ]).as("sort_name"),
              table[:created_at].as("created_at")
            ).arel
          end
          union = selects.reduce { |left, right| Arel::Nodes::UnionAll.new(left, right) }
          Arel::Nodes::TableAlias.new(Arel::Nodes::Grouping.new(union), "secrets")
        end

        # The filtered relation of each selected kind, keyed by kind slug.
        def filtered_scopes
          kinds = selected_kinds
          query = params[:q].to_s.strip
          granted = granted_secret_ids
          kinds.to_h do |kind, config|
            model = config.fetch(:model)
            scope = model.all
            excluded = granted&.fetch(:"#{kind}_secret_id", nil)
            scope = scope.where.not(id: excluded) if excluded.present?
            if query.present?
              pattern = "%#{model.sanitize_sql_like(query)}%"
              scope = scope.where("name ILIKE :pattern OR foreign_id ILIKE :pattern", pattern: pattern)
            end
            [ kind, scope ]
          end
        end

        def selected_kinds
          kind = params[:kind].presence
          return SecretKinds::SECRET_KINDS unless kind
          raise ActionController::BadRequest, "kind must be one of #{SecretKinds::SECRET_KINDS.keys.join(", ")}" unless SecretKinds::SECRET_KINDS.key?(kind)

          SecretKinds::SECRET_KINDS.slice(kind)
        end

        # Secret ids already granted to the exclude_grantee, keyed by grant column,
        # or nil when no grantee filter was given.
        def granted_secret_ids
          grantee = principal_or_role_param!(:exclude_grantee)
          return unless grantee

          columns = Grant::GRANTABLE_ASSOCIATIONS.map { |assoc| :"#{assoc}_id" }
          grantee.grants.pluck(*columns).each_with_object(Hash.new { |h, k| h[k] = [] }) do |row, ids|
            columns.zip(row).each { |column, id| ids[column] << id if id }
          end
        end

        def secret_payload(secret)
          GrantableSummary.call(secret).merge(
            grant_key: :"#{GrantableSummary.kind_for(secret)}_secret_id",
            description: secret.description,
            labels: secret.labels,
            source_types: SecretConsoleView.source_types(secret),
            managed_by: SecretConsoleView.managed_by(secret),
            created_at: secret.created_at,
            updated_at: secret.updated_at
          )
        end

        def detail_payload(secret)
          kind = GrantableSummary.kind_for(secret)
          secret_payload(secret).merge(
            editable: SecretKinds::SECRET_KINDS.dig(kind, :form) == true,
            injection: SecretConsoleView.injection(secret),
            source_segments: SecretConsoleView.source_segments(secret),
            configuration: SecretConsoleView.configuration(secret),
            signed_headers: SecretConsoleView.signed_headers(secret),
            rules: SecretConsoleView.rules(secret)
          )
        end
      end
    end
  end
end
