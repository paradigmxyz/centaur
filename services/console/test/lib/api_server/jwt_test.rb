require "test_helper"

class ApiServerJwtTest < ActiveSupport::TestCase
  test "Principal token materializes sandbox API capabilities" do
    principal = principals(:acme_channel)
    principal.update!(
      sandbox_sessions_read_enabled: false,
      sandbox_workflows_read_enabled: true,
      sandbox_workflows_write_enabled: true
    )

    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      claims = CentaurJwt::Hs256.decode(
        ApiServer::Jwt.encode_for_principal(principal),
        signing_secret: "test-secret",
        iss: ApiServer::Jwt::DEFAULT_ISSUER,
        aud: ApiServer::Jwt::DEFAULT_AUDIENCE
      )

      assert_equal false, claims.dig("capabilities", "sessions_read")
      assert_equal true, claims.dig("capabilities", "workflows_read")
      assert_equal true, claims.dig("capabilities", "workflows_write")
    end
  end

  test "Console service token is purpose bound and short lived" do
    now = Time.current

    with_env("CENTAUR_JWT_SIGNING_SECRET" => "test-secret") do
      token = ApiServer::Jwt.encode_for_console_service(now: now)
      claims = CentaurJwt::Hs256.decode(
        token,
        signing_secret: "test-secret",
        iss: ApiServer::Jwt::DEFAULT_ISSUER,
        aud: ApiServer::Jwt::DEFAULT_AUDIENCE
      )

      assert_equal ApiServer::Jwt::CONSOLE_SERVICE_SUBJECT, claims.fetch("sub")
      assert_equal "console_service", claims.fetch("token_use")
      assert_equal ApiServer::Jwt::DEFAULT_TTL_SECONDS, claims.fetch("exp") - claims.fetch("iat")
    end
  end

  test "Console service token is unavailable without the signing secret" do
    with_env("CENTAUR_JWT_SIGNING_SECRET" => nil) do
      assert_nil ApiServer::Jwt.encode_for_console_service
    end
  end
end
