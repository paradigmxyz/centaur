require "test_helper"

class ApiServer::JwtTest < ActiveSupport::TestCase
  test "console user token carries only the authenticated subject and server admin decision" do
    user = users(:acme_admin)

    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      claims = jwt_payload(ApiServer::Jwt.encode_for_console_user(user, admin: true))

      assert_equal user.oid, claims.fetch("sub")
      assert_equal true, claims.fetch("centaur_admin")
      assert_equal ApiServer::Jwt::DEFAULT_AUDIENCE, claims.fetch("aud")
      assert_equal ApiServer::Jwt::DEFAULT_ISSUER, claims.fetch("iss")
      assert_nil claims["slack"]
    end
  end

  test "console user token records a descoped administrator as non-admin" do
    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      claims = jwt_payload(ApiServer::Jwt.encode_for_console_user(users(:acme_admin), admin: false))

      assert_equal false, claims.fetch("centaur_admin")
    end
  end

  private

  def jwt_payload(token)
    _header, payload, _signature = token.split(".")
    JSON.parse(Base64.urlsafe_decode64(payload))
  end
end
