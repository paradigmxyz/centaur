module Api
  module V1
    class AwsAuthSecretsController < Api::BaseController
      # The credential roles parsed from named top-level keys in the request body.
      CREDENTIAL_ROLES = %w[access_key_id secret_access_key session_token].freeze

      def index
        records, meta = paginated_label_search(AwsAuthSecret.all)
        render json: { data: records.map { |r| record_payload(r) }, meta: meta }
      end

      def show
        ref = AwsAuthSecret.find_by_oid!(params[:id])
        render json: { data: record_payload(ref) }
      end

      # GET /api/v1/aws_auth_secrets/lookup/:namespace/:foreign_id
      def lookup
        render json: { data: record_payload(find_by_foreign_id!(AwsAuthSecret)) }
      end

      def create
        ref = AwsAuthSecret.new(created_by: current_user)
        assign_and_save!(ref, data_params)
        render status: :created, json: { data: record_payload(ref) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      # PUT/PATCH upserts: an opaque id updates that record, any other identifier
      # is a foreign_id that is created when absent.
      def update
        ref = resolve_for_upsert(AwsAuthSecret)
        was_new = ref.new_record?
        assign_and_save!(ref, data_params)
        render status: (was_new ? :created : :ok), json: { data: record_payload(ref) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      # Destroying a secret cascades to its nested sources, rules, and any grants
      # that reference it (dependent: :destroy).
      def destroy
        ref = AwsAuthSecret.find_by_oid!(params[:id])
        ref.destroy!
        head :no_content
      end

      private

      # Builds the whole credential graph in memory and saves once so the
      # credential and rule validations see every source at validation time.
      def assign_and_save!(ref, attrs)
        base = permit_document(ref, attrs, :name, :description,
                               labels: {}, allowed_regions: [], allowed_services: [])

        sources = build_credential_sources(attrs)
        rules_attrs = build_rules(attrs)

        AwsAuthSecret.transaction do
          ref.assign_attributes(base)
          ref.sources = sources
          ref.rules = rules_attrs
          ref.save!
          ref.reload
        end
      end

      # One source per present credential role (access_key_id, secret_access_key,
      # session_token), each a named top-level key holding a {source_type, config}
      # block.
      def build_credential_sources(attrs)
        CREDENTIAL_ROLES.filter_map do |role|
          src = attrs[role]
          next if src.blank?
          SecretSource.new(permit_source(src).merge(role: role, role_kind: "credential_field"))
        end
      end

      def permit_source(src)
        params = src.is_a?(ActionController::Parameters) ? src : ActionController::Parameters.new(src)
        params.permit(:source_type, :secret, config: {}).to_h
      end

      def build_rules(attrs)
        Array(attrs[:rules]).each_with_index.map do |r, i|
          permitted = ActionController::Parameters.new(r.to_unsafe_h).permit(:host, :cidr, http_methods: [], paths: [])
          RequestRule.new(permitted.to_h.merge(position: i))
        end
      end

      def record_payload(ref)
        by_role = ref.sources.index_by(&:role)
        {
          id: ref.oid,
          namespace: ref.namespace,
          foreign_id: ref.foreign_id,
          name: ref.name,
          description: ref.description,
          labels: ref.labels,
          allowed_regions: ref.allowed_regions,
          allowed_services: ref.allowed_services,
          access_key_id: by_role["access_key_id"] && source_payload(by_role["access_key_id"]),
          secret_access_key: by_role["secret_access_key"] && source_payload(by_role["secret_access_key"]),
          session_token: by_role["session_token"] && source_payload(by_role["session_token"]),
          rules: ref.rules.map do |r|
            { host: r.host, cidr: r.cidr, position: r.position, http_methods: r.http_methods, paths: r.paths }
          end,
          created_at: ref.created_at,
          updated_at: ref.updated_at
        }
      end

      def source_payload(source)
        { source_type: source.source_type, config: source.config }
      end
    end
  end
end
