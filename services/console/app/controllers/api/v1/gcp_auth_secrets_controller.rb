module Api
  module V1
    class GcpAuthSecretsController < Api::BaseController
      accepts_credentials :bearer_or_session

      include SecretDocumentWrites

      def index
        records, meta = paginated_label_search(GcpAuthSecret.all)
        render json: { data: records.map { |r| record_payload(r) }, meta: meta }
      end

      def show
        ref = GcpAuthSecret.find_by_oid!(params[:id])
        render json: { data: record_payload(ref) }
      end

      # GET /api/v1/gcp_auth_secrets/lookup/:foreign_id
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
        GcpAuthSecret.transaction do
          # Lock (and reload) first so a retained inline keyfile is read after
          # any concurrent rotation of this secret commits.
          lock_secret_document!(ref)
          base = permit_document(ref, attrs, :name, :description, :subject,
                                 labels: {}, credentials_provider: {}, scopes: [])

          # A PATCH that omits an inline keyfile's `secret` keeps the stored value.
          keyfile_source = build_secret_source(attrs, :keyfile, ref.keyfile_source)
          rules_attrs = build_rules(attrs)
          validate_nested_secret_records!(
            ref, sources: { keyfile: keyfile_source }, rules: rules_attrs,
            source_associations: { keyfile: :keyfile_source }, legacy: :association,
            prepare: lambda {
              ref.assign_attributes(base)
              ref.keyfile_source = keyfile_source
              ref.rules = rules_attrs
            }
          )

          with_sync_config_replacement_guard(ref, base, keyfile_source: keyfile_source, rules: rules_attrs) do
            ref.assign_attributes(base)
            ref.keyfile_source = keyfile_source
            ref.rules = rules_attrs
            ref.save!
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
