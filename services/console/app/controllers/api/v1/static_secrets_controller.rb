module Api
  module V1
    class StaticSecretsController < Api::BaseController
      def index
        records, meta = paginated_label_search(StaticSecret.all)
        render json: { data: records.map { |r| record_payload(r) }, meta: meta }
      end

      def show
        ref = StaticSecret.find_by_oid!(params[:id])
        render json: { data: record_payload(ref) }
      end

      # GET /api/v1/static_secrets/lookup/:namespace/:foreign_id
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
        ref = resolve_for_upsert(StaticSecret)
        was_new = ref.new_record?
        assign_and_save!(ref, data_params)
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

      def assign_and_save!(ref, attrs)
        ss_attrs = permit_document(
          ref, attrs, :name, :description,
          labels: {}, inject_config: {}, replace_config: {}
        )

        source_attrs = if attrs.key?(:source) && attrs[:source].present?
          attrs.require(:source).permit(:source_type, :secret, config: {})
        end

        rules_attrs = Array(attrs[:rules]).map do |r|
          ActionController::Parameters.new(r.to_unsafe_h).permit(
            :host, :cidr, http_methods: [], paths: []
          )
        end

        StaticSecret.transaction do
          ref.assign_attributes(ss_attrs)
          ref.save!

          ref.source&.destroy!
          if source_attrs
            SecretSource.create!(source_attrs.to_h.merge(static_secret: ref))
          end

          ref.rules.destroy_all
          rules_attrs.each_with_index do |r, i|
            RequestRule.create!(r.to_h.merge(position: i, static_secret: ref))
          end

          ref.reload
        end
      end

      def record_payload(ref)
        {
          id: ref.oid,
          namespace: ref.namespace,
          foreign_id: ref.foreign_id,
          name: ref.name,
          description: ref.description,
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
