module Api
  module V1
    class GcpAuthSecretsController < Api::BaseController
      def index
        records, meta = paginated_label_search(GcpAuthSecret.all)
        render json: { data: records.map { |r| record_payload(r) }, meta: meta }
      end

      def show
        ref = GcpAuthSecret.find_by_oid!(params[:id])
        render json: { data: record_payload(ref) }
      end

      # GET /api/v1/gcp_auth_secrets/lookup/:namespace/:foreign_id
      def lookup
        render json: { data: record_payload(find_by_foreign_id!(GcpAuthSecret)) }
      end

      def create
        ref = GcpAuthSecret.new(created_by: current_user)
        assign_and_save!(ref, data_params)
        render status: :created, json: { data: record_payload(ref) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      # PUT/PATCH upserts: an opaque id updates that record, any other identifier
      # is a foreign_id that is created when absent.
      def update
        ref = resolve_for_upsert(GcpAuthSecret)
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
        ref = GcpAuthSecret.find_by_oid!(params[:id])
        ref.destroy!
        head :no_content
      end

      private

      # Builds the whole credential graph in memory and saves once so the
      # cross-record validations (exactly_one_credential) see the keyfile source.
      def assign_and_save!(ref, attrs)
        base = permit_document(ref, attrs, :name, :description, :subject,
                               labels: {}, credentials_provider: {}, scopes: [])

        keyfile_attrs = if attrs.key?(:keyfile) && attrs[:keyfile].present?
          attrs.require(:keyfile).permit(:source_type, :secret, config: {})
        end

        rules_attrs = build_rules(attrs)

        GcpAuthSecret.transaction do
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
          credentials_provider: ref.credentials_provider,
          subject: ref.subject,
          scopes: ref.scopes,
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
