require "test_helper"

class CentaurJwtHs256Test < ActiveSupport::TestCase
  test "encode raises when the signing secret is missing or whitespace" do
    assert_raises(KeyError) { CentaurJwt::Hs256.encode({ "sub" => "x" }, signing_secret: nil) }
    assert_raises(KeyError) { CentaurJwt::Hs256.encode({ "sub" => "x" }, signing_secret: "") }
    assert_raises(KeyError) { CentaurJwt::Hs256.encode({ "sub" => "x" }, signing_secret: "   ") }
  end

  test "encode signs with HS256" do
    token = CentaurJwt::Hs256.encode({ "sub" => "x" }, signing_secret: "test-secret")
    header, payload, signature = token.split(".")

    assert_equal({ "alg" => "HS256", "typ" => "JWT" }, JSON.parse(Base64.urlsafe_decode64(header)))
    assert_equal({ "sub" => "x" }, JSON.parse(Base64.urlsafe_decode64(payload)))
    expected = OpenSSL::HMAC.digest("SHA256", "test-secret", "#{header}.#{payload}")
    assert_equal Base64.urlsafe_encode64(expected, padding: false), signature
  end
end
