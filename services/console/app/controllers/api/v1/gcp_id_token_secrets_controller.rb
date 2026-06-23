module Api
  module V1
    class GcpIdTokenSecretsController < Api::BaseController
      def index
        records, meta = paginated_label_search(GcpIdTokenSecret.all)
        render json: { data: records.map { |r| record_payload(r) }, meta: meta }
      end

      def show
        ref = GcpIdTokenSecret.find_by_oid!(params[:id])
        render json: { data: record_payload(ref) }
      end

      def lookup
        render json: { data: record_payload(find_by_foreign_id!(GcpIdTokenSecret)) }
      end

      def create
        ref = GcpIdTokenSecret.new(created_by: current_user)
        assign_and_save!(ref, data_params)
        render status: :created, json: { data: record_payload(ref) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      def update
        ref = resolve_for_upsert(GcpIdTokenSecret)
        was_new = ref.new_record?
        assign_and_save!(ref, data_params)
        render status: (was_new ? :created : :ok), json: { data: record_payload(ref) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      def destroy
        ref = GcpIdTokenSecret.find_by_oid!(params[:id])
        ref.destroy!
        head :no_content
      end

      private

      def assign_and_save!(ref, attrs)
        base = permit_document(ref, attrs, :name, :description, :audience, :header, labels: {})

        keyfile_attrs = if attrs.key?(:keyfile) && attrs[:keyfile].present?
          attrs.require(:keyfile).permit(:source_type, :secret, config: {})
        end

        rules_attrs = build_rules(attrs)

        GcpIdTokenSecret.transaction do
          ref.assign_attributes(base)
          ref.keyfile_source = keyfile_attrs ? SecretSource.new(keyfile_attrs.to_h) : nil
          ref.rules = rules_attrs
          ref.save!
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
          audience: ref.audience,
          header: ref.header,
          keyfile: ref.keyfile_source && {
            source_type: ref.keyfile_source.source_type,
            config: ref.keyfile_source.config
          },
          rules: ref.rules.map do |r|
            { host: r.host, cidr: r.cidr, position: r.position, http_methods: r.http_methods, paths: r.paths }
          end,
          created_at: ref.created_at,
          updated_at: ref.updated_at
        }
      end
    end
  end
end
