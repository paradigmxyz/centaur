module Api
  module V1
    class GrantsController < Api::BaseController
      def show
        grant = Grant.find_by_oid!(params[:id])
        render json: serialize(grant)
      end

      GRANTEE_TYPES = {
        principal_id: Principal,
        role_id: Role
      }.freeze

      GRANTABLE_TYPES = {
        static_secret_id: StaticSecret,
        gcp_auth_secret_id: GcpAuthSecret,
        aws_auth_secret_id: AwsAuthSecret,
        oauth_token_secret_id: OauthTokenSecret,
        pg_dsn_secret_id: PgDsnSecret,
        hmac_secret_id: HmacSecret
      }.freeze

      def create
        attrs = data_params.permit(*GRANTEE_TYPES.keys, *GRANTABLE_TYPES.keys)

        grantee_key, grantee = resolve_one(attrs, GRANTEE_TYPES)
        return render_missing(GRANTEE_TYPES) unless grantee_key

        grantable_key, grantable = resolve_one(attrs, GRANTABLE_TYPES)
        return render_missing(GRANTABLE_TYPES) unless grantable_key

        # Granting the same secret to the same grantee is idempotent: a repeat
        # returns the existing grant with 200 rather than a duplicate row. A
        # unique index backs this, so a race that slips past find_or_create_by
        # raises RecordNotUnique, which we resolve to the now-existing grant.
        identity = { assoc(grantee_key) => grantee, assoc(grantable_key) => grantable }
        grant = Grant.create_with(created_by: current_user).find_or_create_by!(identity)
        render status: (grant.previously_new_record? ? :created : :ok), json: serialize(grant)
      rescue ActiveRecord::RecordNotUnique
        grant = Grant.find_by!(identity)
        render status: :ok, json: serialize(grant)
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      def destroy
        grant = Grant.find_by_oid!(params[:id])
        grant.destroy!
        head :no_content
      end

      private

      # Picks the single present key from a {param_key => Model} map and loads the
      # referenced record (404 if it does not exist). Returns nil when no key is
      # present; the caller turns that into a validation error.
      def resolve_one(attrs, types)
        key = types.keys.find { |k| attrs[k].present? }
        return nil unless key
        [ key, types.fetch(key).find_by_oid!(attrs[key]) ]
      end

      def assoc(param_key)
        param_key.to_s.delete_suffix("_id").to_sym
      end

      def render_missing(types)
        render status: :unprocessable_entity, json: {
          error: { message: "validation failed",
                   details: { base: [ "must reference one of #{types.keys.join(", ")}" ] } }
        }
      end

      def serialize(grant)
        grantee = grant.grantee
        grantable = grant.grantable
        {
          data: {
            id: grant.oid,
            GRANTEE_TYPES.key(grantee.class) => grantee.oid,
            GRANTABLE_TYPES.key(grantable.class) => grantable.oid,
            created_at: grant.created_at,
            updated_at: grant.updated_at
          }
        }
      end
    end
  end
end
