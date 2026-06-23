module Api
  module V1
    class OauthTokenSecretsController < Api::BaseController
      def index
        records, meta = paginated_label_search(OauthTokenSecret.all)
        render json: { data: records.map { |r| record_payload(r) }, meta: meta }
      end

      def show
        ref = OauthTokenSecret.find_by_oid!(params[:id])
        render json: { data: record_payload(ref) }
      end

      # GET /api/v1/oauth_token_secrets/lookup/:namespace/:foreign_id
      def lookup
        render json: { data: record_payload(find_by_foreign_id!(OauthTokenSecret)) }
      end

      def create
        ref = OauthTokenSecret.new(created_by: current_user)
        assign_and_save!(ref, data_params)
        render status: :created, json: { data: record_payload(ref) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      # PUT/PATCH upserts: an opaque id updates that record, any other identifier
      # is a foreign_id that is created when absent.
      def update
        ref = resolve_for_upsert(OauthTokenSecret)
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
        ref = OauthTokenSecret.find_by_oid!(params[:id])
        ref.destroy!
        head :no_content
      end

      private

      # Builds the whole credential graph in memory and saves once so the
      # per-grant field validations see every credential source at validation time.
      def assign_and_save!(ref, attrs)
        base = permit_document(ref, attrs, :name, :description,
                               :grant, :token_endpoint, :audience, :header, :value_prefix,
                               labels: {}, scopes: [])

        sources = build_credential_sources(attrs) + build_header_sources(attrs)
        rules_attrs = build_rules(attrs)

        OauthTokenSecret.transaction do
          ref.assign_attributes(base)
          ref.sources = sources
          ref.rules = rules_attrs
          ref.save!
          ref.reload
        end
      end

      def build_credential_sources(attrs)
        return [] unless attrs.key?(:credentials) && attrs[:credentials].present?

        attrs.require(:credentials).each_pair.map do |role, src|
          SecretSource.new(permit_source(src).merge(role: role.to_s, role_kind: "credential_field"))
        end
      end

      def build_header_sources(attrs)
        return [] unless attrs.key?(:token_endpoint_headers) && attrs[:token_endpoint_headers].present?

        attrs.require(:token_endpoint_headers).each_pair.map do |name, src|
          SecretSource.new(permit_source(src).merge(role: name.to_s, role_kind: "endpoint_header"))
        end
      end

      def permit_source(src)
        params = src.is_a?(ActionController::Parameters) ? src : ActionController::Parameters.new(src)
        params.permit(:source_type, :secret, config: {}).to_h
      end

      def record_payload(ref)
        cred = ref.sources.select(&:credential_field?)
        headers = ref.sources.select(&:endpoint_header?)
        {
          id: ref.oid,
          namespace: ref.namespace,
          foreign_id: ref.foreign_id,
          name: ref.name,
          description: ref.description,
          labels: ref.labels,
          grant: ref.grant,
          token_endpoint: ref.token_endpoint,
          audience: ref.audience,
          scopes: ref.scopes,
          header: ref.header,
          value_prefix: ref.value_prefix,
          credentials: cred.to_h { |s| [ s.role, source_payload(s) ] },
          token_endpoint_headers: headers.to_h { |s| [ s.role, source_payload(s) ] },
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
