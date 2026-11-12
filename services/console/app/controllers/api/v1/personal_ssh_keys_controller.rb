module Api
  module V1
    # Self-service management of the caller's personal SSH key
    # (/api/v1/me/ssh_key). The key is an ordinary SshKeySecret owned by the
    # caller; saving it grants it to the caller's own principals
    # (PersonalSshKeyGrants). There is no id in the path, so a caller can only
    # ever reach their own key. The private key is write-only: no response,
    # including validation errors, ever contains it.
    class PersonalSshKeysController < Api::BaseController
      accepts_credentials :session

      DEFAULT_NAME = "My SSH key".freeze

      allow_active_users
      before_action :no_store
      before_action :load_key

      def show
        render json: { data: key_payload }
      end

      def update
        attributes = params.require(:data).permit(:name, :private_key)
        @key.name = attributes[:name] if attributes.key?(:name)
        @key.private_key = attributes[:private_key] if attributes[:private_key].present?
        return render_validation_error(@key) unless @key.save

        # Idempotent: saving again also repairs grants a failed earlier
        # enrollment never created.
        PersonalSshKeyGrants.new.apply_for_user(current_user)
        render json: { data: key_payload }
      end

      # Deleting the key removes every grant of it, so access ends at the
      # proxy's next authorization check.
      def destroy
        @key.destroy! if @key.persisted?
        head :no_content
      end

      # Checks a pasted private key without saving it.
      def validate
        public_key = SshKey.public_key(params.require(:data)[:private_key].to_s)
        render json: {
          data: { valid: true, message: "Valid private key. Ready to save.", fingerprint: SshKey.fingerprint(public_key) }
        }
      rescue SshKey::Invalid => error
        # SshKey messages never include the submitted key.
        render_error(status: :unprocessable_entity, message: error.message,
                     details: { valid: false, private_key: [ error.message ] })
      end

      private

      def no_store
        response.headers["Cache-Control"] = "no-store"
      end

      def load_key
        @key = current_user.personal_ssh_key ||
          current_user.build_personal_ssh_key(name: DEFAULT_NAME, created_by: current_user)
      end

      def key_payload
        saved = @key.persisted?
        {
          id: saved ? @key.oid : nil,
          saved: saved,
          name: saved ? @key.name : DEFAULT_NAME,
          has_private_key: saved && @key.key_source.present?,
          fingerprint: saved ? @key.key_fingerprint : nil,
          public_key: saved ? @key.public_key : nil,
          created_at: saved ? @key.created_at.iso8601 : nil,
          updated_at: saved ? @key.updated_at.iso8601 : nil,
          grants: saved ? grants_payload : []
        }
      end

      def grants_payload
        @key.grants.includes(:principal, :role).order(:id).map do |grant|
          grantee = grant.grantee
          {
            id: grant.oid,
            grantee_type: grant.role ? "role" : "principal",
            grantee_id: grantee.oid,
            label: grantee.try(:name).presence || grantee.foreign_id.presence || grantee.oid,
            principal_kind: grant.principal&.kind
          }
        end
      end
    end
  end
end
