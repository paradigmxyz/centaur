require "test_helper"

module Login
  class OidcDiscoveryTest < ActiveSupport::TestCase
    test "normalizes a valid HTTPS issuer" do
      assert_equal "https://identity.example.com/oauth2/default",
                   OidcDiscovery.normalized_issuer("https://identity.example.com/oauth2/default/")
    end

    test "rejects an insecure issuer" do
      error = assert_raises(Broker::ExchangeError) do
        OidcDiscovery.normalized_issuer("http://identity.example.com")
      end
      assert_equal "oidc_issuer_invalid", error.code
    end

    test "rejects a discovered endpoint on another origin" do
      error = assert_raises(Broker::ExchangeError) do
        OidcDiscovery.validate_endpoint!(
          "https://attacker.example/v1/keys",
          issuer: "https://identity.example.com"
        )
      end
      assert_equal "oidc_endpoint_invalid", error.code
    end

    test "classifies discovery network failures without exposing transport details" do
      client = Object.new
      client.define_singleton_method(:get) { |_url| raise Net::OpenTimeout, "private detail" }

      error = HttpClient.stub(:new, client) do
        assert_raises(Broker::ExchangeError) do
          OidcDiscovery.fetch_json("https://identity.example.com/.well-known/openid-configuration")
        end
      end

      assert_equal "oidc_document_failed", error.code
      assert_not_includes error.message, "private detail"
    end
  end
end
