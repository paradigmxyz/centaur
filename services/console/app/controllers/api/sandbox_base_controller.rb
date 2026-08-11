module Api
  class SandboxBaseController < ActionController::API
    include ApiRequestSupport

    before_action :authenticate_sandbox_token!

    rescue_from ActiveRecord::RecordNotFound, with: :render_not_found
    rescue_from ActiveRecord::RecordInvalid, with: :render_record_invalid
    rescue_from ActiveRecord::RecordNotUnique, with: :render_record_not_unique
    rescue_from ActiveRecord::StaleObjectError, with: :render_stale_object

    attr_reader :current_proxy, :sandbox_claims

    private

    def authenticate_sandbox_token!
      token = bearer_token
      return render_error(status: :unauthorized, message: "invalid or missing sandbox token") if token.blank?

      claims = SandboxEntitlements::Jwt.decode(token)
      proxy = Proxy.find_by_oid(claims["proxy_id"])
      valid = proxy&.assigned? && proxy.principal&.oid == claims["principal_id"] &&
        proxy.name == claims["sandbox_id"]
      return render_error(status: :unauthorized, message: "invalid sandbox token") unless valid

      @sandbox_claims = claims
      @current_proxy = proxy
    rescue CentaurJwt::Hs256::VerificationError
      render_error(status: :unauthorized, message: "invalid or missing sandbox token")
    end

    def render_not_found(error)
      render_error(status: :not_found, message: error.message)
    end

    def render_record_invalid(error)
      render_error(
        status: :unprocessable_entity,
        message: "validation failed",
        details: error.record.errors.as_json
      )
    end

    def render_record_not_unique(_error)
      render_error(status: :unprocessable_entity, message: "record conflicts with an existing record")
    end

    def render_stale_object(_error)
      render_error(status: :conflict, message: "record changed since it was loaded")
    end
  end
end
