require "test_helper"

module Login
  module Providers
    class OktaTest < ActiveSupport::TestCase
      ISSUER = "https://identity.example.com/oauth2/default".freeze
      CLIENT_ID = "okta-console-client".freeze
      NONCE = "flow-bound-nonce".freeze

      setup do
        @previous_issuer = ENV["CENTAUR_CONSOLE_OKTA_ISSUER"]
        ENV["CENTAUR_CONSOLE_OKTA_ISSUER"] = ISSUER
        @key = OpenSSL::PKey::RSA.generate(2048)
        @jwk = JWT::JWK.new(@key.public_key, kid: "test-key").export
        @metadata = {
          "issuer" => ISSUER,
          "authorization_endpoint" => "#{ISSUER}/v1/authorize",
          "token_endpoint" => "#{ISSUER}/v1/token",
          "userinfo_endpoint" => "#{ISSUER}/v1/userinfo",
          "jwks_uri" => "#{ISSUER}/v1/keys"
        }
      end

      teardown do
        ENV["CENTAUR_CONSOLE_OKTA_ISSUER"] = @previous_issuer
      end

      test "discovers endpoints and verifies a flow-bound Okta identity" do
        with_discovery do |requests|
          strategy = Okta.new
          assert_equal @metadata["authorization_endpoint"], strategy.authorization_endpoint
          assert_equal @metadata["token_endpoint"], strategy.token_endpoint

          identity = strategy.identity_from(result(token), client_id: CLIENT_ID, nonce: NONCE)
          assert_equal "okta-user-123", identity[:subject]
          assert_equal "operator@example.com", identity[:email]
          assert identity[:email_verified]
          assert_equal "Example Operator", identity[:name]
          assert_equal [
            {
              url: @metadata["userinfo_endpoint"],
              authorization: "Bearer AT"
            }
          ], requests
        end
      end

      test "rejects a token signed by an unknown key" do
        attacker = OpenSSL::PKey::RSA.generate(2048)
        forged = token(signing_key: attacker)

        with_discovery do
          error = assert_raises(Broker::ExchangeError) do
            Okta.new.identity_from(result(forged), client_id: CLIENT_ID, nonce: NONCE)
          end
          assert_equal "id_token_invalid", error.code
        end
      end

      test "rejects a token from another browser flow" do
        with_discovery do
          error = assert_raises(Broker::ExchangeError) do
            Okta.new.identity_from(result(token), client_id: CLIENT_ID, nonce: "another-nonce")
          end
          assert_equal "id_token_nonce_mismatch", error.code
        end
      end

      test "requires a verified email from UserInfo" do
        with_discovery(userinfo: userinfo_claims("email_verified" => false)) do
          error = assert_raises(Broker::ExchangeError) do
            Okta.new.identity_from(result(token), client_id: CLIENT_ID, nonce: NONCE)
          end
          assert_equal "userinfo_email_unverified", error.code
        end
      end

      test "requires UserInfo and id_token subjects to match" do
        with_discovery(userinfo: userinfo_claims("sub" => "another-user")) do
          error = assert_raises(Broker::ExchangeError) do
            Okta.new.identity_from(result(token), client_id: CLIENT_ID, nonce: NONCE)
          end
          assert_equal "userinfo_subject_mismatch", error.code
        end
      end

      private

      def with_discovery(userinfo: userinfo_claims, &block)
        requests = []
        client = Object.new
        client.define_singleton_method(:get) do |url, headers: {}|
          requests << { url: url, authorization: headers["Authorization"] }
          HttpClient::Response.new(status: 200, body: userinfo.to_json, headers: {})
        end

        Login::OidcDiscovery.stub(:metadata, @metadata) do
          Login::OidcDiscovery.stub(:jwks, { keys: [ @jwk ] }) do
            HttpClient.stub(:new, client) { block.call(requests) }
          end
        end
      end

      def userinfo_claims(overrides = {})
        {
          "sub" => "okta-user-123",
          "email" => "operator@example.com",
          "email_verified" => true,
          "name" => "Example Operator"
        }.merge(overrides)
      end

      def token(overrides = {}, signing_key: @key)
        claims = {
          "iss" => ISSUER,
          "aud" => CLIENT_ID,
          "sub" => "okta-user-123",
          "name" => "Example Operator",
          "nonce" => NONCE,
          "iat" => Time.current.to_i,
          "exp" => 5.minutes.from_now.to_i
        }.merge(overrides)
        JWT.encode(claims, signing_key, "RS256", kid: "test-key")
      end

      def result(id_token)
        Broker::AuthorizationCodeClient::Result.new(
          access_token: "AT",
          refresh_token: nil,
          expires_in: 3600,
          scope: "openid email profile",
          id_token: id_token,
          response: {}
        )
      end
    end
  end
end
