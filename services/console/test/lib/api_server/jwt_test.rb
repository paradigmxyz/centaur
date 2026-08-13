require "test_helper"

class ApiServer::JwtTest < ActiveSupport::TestCase
  test "console user token carries authenticated subject server admin decision and Feishu aliases" do
    user = users(:acme_admin)
    user.user_identities.create!(
      provider: "feishu",
      subject: JSON.generate([ "tenant-a", "on-user" ]),
      tenant_key: "tenant-a",
      open_id: "ou-user"
    )

    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      claims = jwt_payload(ApiServer::Jwt.encode_for_console_user(user, admin: true))

      assert_equal user.oid, claims.fetch("sub")
      assert_equal true, claims.fetch("centaur_admin")
      assert_equal ApiServer::Jwt::DEFAULT_AUDIENCE, claims.fetch("aud")
      assert_equal ApiServer::Jwt::DEFAULT_ISSUER, claims.fetch("iss")
      assert_nil claims["slack"]
      assert_equal [ "feishu:tenant-a:ou-user" ], claims.fetch("development_principal_ids")
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
