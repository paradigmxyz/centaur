module Api
  module V1
    # Operator CRUD for managed broker credentials. Mirrors the secret
    # controllers (oid/foreign_id addressing, label search, PUT-upsert), with two
    # differences: initial values are write-only, and the rotating token blob
    # (access_token/refresh_token) is NEVER serialized back.
    class BrokerCredentialsController < Api::BaseController
      def index
        records, meta = paginated_label_search(BrokerCredential.all)
        render json: { data: records.map { |r| record_payload(r) }, meta: meta }
      end

      def show
        ref = BrokerCredential.find_by_oid!(params[:id])
        render json: { data: record_payload(ref) }
      end

      # GET /api/v1/broker_credentials/lookup/:namespace/:foreign_id
      def lookup
        render json: { data: record_payload(find_by_foreign_id!(BrokerCredential)) }
      end

      def create
        ref = BrokerCredential.new(created_by: current_user)
        assign_and_save!(ref, data_params)
        render status: :created, json: { data: record_payload(ref) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      def update
        ref = resolve_for_upsert(BrokerCredential)
        was_new = ref.new_record?
        assign_and_save!(ref, data_params)
        render status: (was_new ? :created : :ok), json: { data: record_payload(ref) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      def destroy
        ref = BrokerCredential.find_by_oid!(params[:id])
        ref.destroy!
        head :no_content
      rescue ActiveRecord::RecordNotDestroyed
        render status: :conflict, json: { error: { message: ref.errors.full_messages.to_sentence } }
      end

      private

      def assign_and_save!(ref, attrs)
        base = attrs.permit(:namespace, :foreign_id, :name, :description, :token_endpoint,
                            :grant, :client_id,
                            :early_refresh_slack_seconds, :early_refresh_fraction,
                            :max_refresh_interval_seconds, :refresh_timeout_seconds,
                            labels: {}, scopes: [])
        # A PUT upsert by foreign_id sets identity before assignment; a blank body
        # value must not wipe it.
        base.delete(:foreign_id) if base[:foreign_id].blank? && ref.foreign_id.present?
        base.delete(:namespace) if base[:namespace].blank? && ref.namespace.present?
        base[:namespace] = "default" if base[:namespace].blank? && ref.namespace.blank?

        BrokerCredential.transaction do
          ref.assign_attributes(base)
          apply_client_secret(ref, attrs)
          apply_token_endpoint_headers(ref, attrs)
          apply_initial_values(ref, attrs)
          ref.save!
          ref.reload
        end
      end

      # token_endpoint_headers is an open map of header name => string value, so it
      # is read as an unsafe hash (the model validates the shape) and only when the
      # body supplies it, so a partial update leaves the existing headers in place.
      def apply_token_endpoint_headers(ref, attrs)
        return unless attrs.key?(:token_endpoint_headers)
        raw = attrs[:token_endpoint_headers]
        ref.token_endpoint_headers = raw.respond_to?(:to_unsafe_h) ? raw.to_unsafe_h : raw
      end

      def apply_client_secret(ref, attrs)
        secret = attrs[:client_secret]
        ref.client_secret = secret if secret.present?
      end

      # These fields are write-only initial/re-auth values. Supplying any
      # fresh value resets the credential to "due now" and clears dead state, so
      # the next poll refreshes it. Blank/absent values leave stored material
      # and rotation state untouched.
      def apply_initial_values(ref, attrs)
        changed = false
        %i[refresh_token username password api_key].each do |field|
          value = attrs[field]
          next if value.blank?

          ref.public_send("#{field}=", value)
          changed = true
        end
        return unless changed

        ref.dead = false
        ref.dead_reason = nil
        ref.failure_count = 0
        ref.next_attempt_at = Time.current
      end

      # Observability only. The client_secret, username/password/api_key, the
      # token_endpoint_headers values, the minted access_token, and the
      # refresh_token are deliberately never included; only the header names are
      # surfaced.
      def record_payload(ref)
        {
          id: ref.oid,
          namespace: ref.namespace,
          foreign_id: ref.foreign_id,
          name: ref.name,
          description: ref.description,
          labels: ref.labels,
          grant: ref.grant,
          token_endpoint: ref.token_endpoint,
          scopes: ref.scopes,
          client_id: ref.client_id,
          token_endpoint_header_names: (ref.token_endpoint_headers || {}).keys,
          early_refresh_slack_seconds: ref.early_refresh_slack_seconds,
          early_refresh_fraction: ref.early_refresh_fraction,
          max_refresh_interval_seconds: ref.max_refresh_interval_seconds,
          refresh_timeout_seconds: ref.refresh_timeout_seconds,
          status: ref.status,
          expires_at: ref.expires_at,
          last_refresh: ref.last_refresh,
          next_attempt_at: ref.next_attempt_at,
          dead: ref.dead,
          dead_reason: ref.dead_reason,
          failure_count: ref.failure_count,
          # Provenance for flow-minted credentials (nil for standalone ones).
          oauth_app_id: ref.oauth_app&.oid,
          provider_subject: ref.provider_subject,
          provider_email: ref.provider_email,
          external_user_key: ref.external_user_key,
          created_at: ref.created_at,
          updated_at: ref.updated_at
        }
      end
    end
  end
end
