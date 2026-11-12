module Api
  module V1
    # Operator-managed SSH keys, granted through /api/v1/grants like any other
    # secret. Personal keys (those with an owner) are readable and deletable
    # here but are written only by their owner from the Integrations page.
    class SshKeySecretsController < Api::BaseController
      accepts_credentials :bearer_or_session

      def index
        records, meta = paginated_label_search(SshKeySecret.includes(:key_source))
        render json: { data: records.map { |r| record_payload(r) }, meta: meta }
      end

      def show
        render json: { data: record_payload(SshKeySecret.find_by_oid!(params[:id])) }
      end

      # GET /api/v1/ssh_key_secrets/lookup/:foreign_id
      def lookup
        render json: { data: record_payload(find_by_foreign_id!(SshKeySecret)) }
      end

      def create
        ref = SshKeySecret.new(created_by: current_user)
        assign_and_save!(ref, data_params)
        render status: :created, json: { data: record_payload(ref) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      # PUT/PATCH upserts: an opaque id updates that record, any other identifier
      # is a foreign_id that is created when absent.
      def update
        ref = resolve_for_upsert(SshKeySecret)
        return render_personal_key_forbidden if ref.personal?

        was_new = ref.new_record?
        assign_and_save!(ref, data_params)
        render status: (was_new ? :created : :ok), json: { data: record_payload(ref) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      # Destroying the key cascades to its source and every grant of it.
      def destroy
        SshKeySecret.find_by_oid!(params[:id]).destroy!
        head :no_content
      end

      private

      def assign_and_save!(ref, attrs)
        base = permit_document(ref, attrs, :name, :description, :public_key, labels: {})
        source_attrs = if attrs.key?(:private_key) && attrs[:private_key].present?
          attrs.require(:private_key).permit(:source_type, :secret, config: {})
        end
        source_attrs = source_attrs&.to_h
        if source_attrs&.dig("source_type") == "control_plane"
          # An inline key's public key is derived, and its value is stored
          # normalized. Compare the replacement in that same form so replaying
          # an identical document is a no-op that keeps live connections.
          base.delete(:public_key)
          source_attrs["secret"] = SshKey.normalize(source_attrs["secret"]) if source_attrs["secret"].present?
        end
        key_source = source_attrs ? SecretSource.new(source_attrs) : nil

        # Build the whole graph and save once so the key validations see the
        # source. Assigning the has_one replaces any prior source; a PUT is a
        # full replace, and a no-op PUT leaves active connections untouched.
        SshKeySecret.transaction do
          with_sync_config_replacement_guard(ref, base, key_source: key_source) do
            ref.assign_attributes(base)
            ref.key_source = key_source
            ref.save!
            ref.reload
          end
        end
      end

      def render_personal_key_forbidden
        render status: :forbidden, json: {
          error: { message: "personal SSH keys are managed by their owner" }
        }
      end

      def record_payload(ref)
        {
          id: ref.oid,
          foreign_id: ref.foreign_id,
          name: ref.name,
          description: ref.description,
          labels: ref.labels,
          public_key: ref.public_key,
          personal: ref.personal?,
          private_key: ref.key_source && {
            source_type: ref.key_source.source_type,
            config: ref.key_source.config
          },
          created_at: ref.created_at,
          updated_at: ref.updated_at
        }
      end
    end
  end
end
