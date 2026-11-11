module Api
  module V1
    class StaticSecretsController < Api::BaseController
      include SecretDocumentWrites

      def index
        records, meta = paginated_label_search(StaticSecret.all)
        render json: { data: records.map { |r| record_payload(r) }, meta: meta }
      end

      def show
        ref = StaticSecret.find_by_oid!(params[:id])
        render json: { data: record_payload(ref) }
      end

      # GET /api/v1/static_secrets/lookup/:foreign_id
      def lookup
        render json: { data: record_payload(find_by_foreign_id!(StaticSecret)) }
      end

      def create
        ref = StaticSecret.new(created_by: current_user)
        assign_and_save!(ref, data_params)
        render status: :created, json: { data: record_payload(ref) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      # PUT/PATCH upserts: an opaque id updates that record, any other identifier
      # is a foreign_id that is created when absent.
      def update
        ref, was_new = assign_upsert_with_retry
        render status: (was_new ? :created : :ok), json: { data: record_payload(ref) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      # Destroying a secret cascades to its nested sources, rules, and any
      # grants that reference it (dependent: :destroy), so the role and
      # principal associations are removed without touching the roles or
      # principals themselves.
      def destroy
        ref = StaticSecret.find_by_oid!(params[:id])
        ref.destroy!
        head :no_content
      end

      private

      def assign_upsert_with_retry
        attempts = 0

        begin
          attempts += 1
          ref = resolve_for_upsert(StaticSecret)
          was_new = ref.new_record?
          assign_and_save!(ref, data_params)
          [ ref, was_new ]
        rescue ActiveRecord::RecordNotUnique
          raise if attempts >= 2

          retry
        end
      end

      def assign_and_save!(ref, attrs)
        ss_attrs = permit_document(
          ref, attrs, :name, :description, :kind,
          labels: {}, inject_config: {}, replace_config: {}
        )
        # Older clients do not know about kind. Preserve it on update when the
        # field is absent, while creates still receive the database default and
        # an explicitly supplied kind still replaces the existing value.
        ss_attrs[:kind] = ref.kind if ref.persisted? && !attrs.key?(:kind)

        # A PATCH that omits an inline source's `secret` keeps the stored value.
        source = build_secret_source(attrs, :source, ref.source)
        rules = build_rules(attrs)

        # Resolve profile defaults before the replacement guard so a repeated
        # write compares the effective persisted document, not the abbreviated
        # client request.
        candidate = ref.dup
        candidate.assign_attributes(ss_attrs)
        rules = candidate.apply_kind_defaults(rules: rules)
        ss_attrs = ss_attrs.merge(
          kind: candidate.kind,
          inject_config: candidate.inject_config,
          replace_config: candidate.replace_config
        )

        StaticSecret.transaction do
          with_sync_config_replacement_guard(ref, ss_attrs, source: source, rules: rules) do
            ref.assign_attributes(ss_attrs)
            ref.kind_rules_for_validation = rules
            ref.save!

            ref.source&.destroy!
            ref.rules.destroy_all
            source&.static_secret = ref
            rules.each { |rule| rule.static_secret = ref }
            # Invalid nested records are reported (and rolled back) after the
            # parent saved, keeping the error keys of the per-record save!.
            validate_nested_secret_records!(ref, sources: { source: source }, rules: rules, legacy: :flat)

            source&.save!
            rules.each(&:save!)

            ref.reload
          end
        end
      end

      def record_payload(ref)
        {
          id: ref.oid,
          foreign_id: ref.foreign_id,
          name: ref.name,
          description: ref.description,
          kind: ref.kind,
          labels: ref.labels,
          inject_config: ref.inject_config,
          replace_config: ref.replace_config,
          source: ref.source && {
            source_type: ref.source.source_type,
            config: ref.source.config
          },
          rules: ref.rules.map do |r|
            {
              host: r.host,
              cidr: r.cidr,
              position: r.position,
              http_methods: r.http_methods,
              paths: r.paths
            }
          end,
          created_at: ref.created_at,
          updated_at: ref.updated_at
        }
      end
    end
  end
end
