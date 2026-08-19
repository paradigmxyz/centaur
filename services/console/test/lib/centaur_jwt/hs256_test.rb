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

  test "decode verifies signature issuer audience and expiry" do
    token = CentaurJwt::Hs256.encode(
      { "iss" => "issuer", "aud" => "audience", "sub" => "x", "exp" => 1.hour.from_now.to_i },
      signing_secret: "test-secret"
    )

    payload = CentaurJwt::Hs256.decode(
      token,
      signing_secret: "test-secret",
      iss: "issuer",
      aud: "audience"
    )

    assert_equal "x", payload.fetch("sub")
  end

  test "decode rejects invalid signatures" do
    token = CentaurJwt::Hs256.encode({ "sub" => "x" }, signing_secret: "test-secret")
    tampered = token.sub(/\.[^.]+\z/, ".bad")

    assert_raises(CentaurJwt::Hs256::VerificationError) do
      CentaurJwt::Hs256.decode(tampered, signing_secret: "test-secret")
    end
  end

  test "decode rejects structurally malformed tokens" do
    [
      "W10.e30.AAAA",    # header segment decodes to a JSON array
      "bnVsbA.e30.AAAA", # header segment decodes to JSON null
      "not-a-jwt",
      ""
    ].each do |token|
      assert_raises(CentaurJwt::Hs256::VerificationError, token.inspect) do
        CentaurJwt::Hs256.decode(token, signing_secret: "test-secret")
      end
    end
  end

  test "decode rejects issuer and audience mismatches" do
    token = CentaurJwt::Hs256.encode(
      { "iss" => "issuer", "aud" => "audience", "exp" => 1.hour.from_now.to_i },
      signing_secret: "test-secret"
    )

    assert_raises(CentaurJwt::Hs256::VerificationError) do
      CentaurJwt::Hs256.decode(token, signing_secret: "test-secret", iss: "other", aud: "audience")
    end
    assert_raises(CentaurJwt::Hs256::VerificationError) do
      CentaurJwt::Hs256.decode(token, signing_secret: "test-secret", iss: "issuer", aud: "other")
    end
  end

  test "decode rejects expired tokens" do
    token = CentaurJwt::Hs256.encode(
      { "sub" => "x", "exp" => 1.hour.ago.to_i },
      signing_secret: "test-secret"
    )

    assert_raises(CentaurJwt::Hs256::VerificationError) do
      CentaurJwt::Hs256.decode(token, signing_secret: "test-secret")
    end
  end
end
