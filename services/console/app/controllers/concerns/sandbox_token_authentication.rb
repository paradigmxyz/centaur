# Authenticates the sandbox entitlement JWT that a sandbox's iron-proxy injects
# for /api/v1/sandbox/* requests. The proxy -> principal binding is re-checked
# against the database, so reassigning a proxy revokes access immediately.
module SandboxTokenAuthentication
  extend ActiveSupport::Concern

  included do
    attr_reader :current_proxy, :sandbox_claims
  end

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
end
