require "test_helper"

class BrokerCredentialTest < ActiveSupport::TestCase
  def build_credential(refresh_token: "seed-rt", **overrides)
    BrokerCredential.new({
      foreign_id: "cred-#{SecureRandom.hex(4)}",
      token_endpoint: "https://idp.example/token", scopes: %w[a b],
      client_id: "cid", client_secret: "sec",
      created_by: users(:acme_admin), refresh_token: refresh_token
    }.merge(overrides))
  end

  def create_credential(**kw)
    bc = build_credential(**kw)
    bc.save!
    bc
  end

  # --- validations ----------------------------------------------------------

  test "valid with a client_id" do
    assert build_credential.valid?
  end

  test "foreign_id cannot be changed once set" do
    credential = create_credential
    original_foreign_id = credential.foreign_id

    assert_not credential.update(foreign_id: "renamed-#{SecureRandom.hex(4)}")
    assert_includes credential.errors[:foreign_id], "cannot be changed once set"
    assert_equal original_foreign_id, credential.reload.foreign_id
  end

  test "at most one wrapping static secret per credential" do
    cred = create_credential
    StaticSecret.create!(name: "wrapper", broker_credential: cred,
                         inject_config: { "header" => "Authorization" })
    dup = StaticSecret.new(name: "dup", broker_credential: cred,
                           inject_config: { "header" => "Authorization" })
    assert_raises(ActiveRecord::RecordNotUnique) { dup.save!(validate: false) }
  end

  test "invalid without a client_id" do
    bc = build_credential(client_id: nil)
    refute bc.valid?
    assert bc.errors[:client_id].any?
  end

  test "password grant is valid with username and password" do
    bc = build_credential(grant: "password", username: "user", password: "pass", refresh_token: nil)
    assert bc.valid?, bc.errors.full_messages.to_sentence
  end

  test "client_credentials grant is valid with client credentials and no refresh token" do
    bc = build_credential(grant: "client_credentials", refresh_token: nil,
                          client_id: "cid", client_secret: "sec")
    assert bc.valid?, bc.errors.full_messages.to_sentence
  end

  test "client_credentials grant requires a client secret" do
    bc = build_credential(grant: "client_credentials", refresh_token: nil,
                          client_id: "cid", client_secret: nil)
    refute bc.valid?
    assert bc.errors[:client_secret].any? { |m| m.include?("client_credentials grant") }
  end

  test "password grant requires username and password" do
    bc = build_credential(grant: "password", username: "user", password: nil, refresh_token: nil)
    refute bc.valid?
    assert bc.errors[:password].any? { |m| m.include?("password grant") }
  end

  test "preqin grant is valid with username and API key and defaults endpoint" do
    bc = build_credential(grant: "preqin", token_endpoint: nil, client_id: nil,
                          username: "user", api_key: "api-key", refresh_token: nil)
    assert bc.valid?, bc.errors.full_messages.to_sentence
    assert_equal BrokerCredential::PREQIN_TOKEN_ENDPOINT, bc.token_endpoint
  end

  test "preqin grant requires username and API key" do
    bc = build_credential(grant: "preqin", client_id: nil, username: "user",
                          api_key: nil, refresh_token: nil)
    refute bc.valid?
    assert bc.errors[:api_key].any? { |m| m.include?("Preqin broker grant") }
  end

  # --- oauth_app provenance (flow-minted credentials) -----------------------

  def build_app(**overrides)
    OauthApp.create!({
      provider: "google", slug: "slug-#{SecureRandom.hex(4)}",
      client_id: "app-cid", client_secret: "app-secret",
      allowed_scopes: %w[a b],
      created_by: users(:acme_admin)
    }.merge(overrides))
  end

  test "client_id not required when linked to an oauth_app" do
    app = build_app
    bc = build_credential(client_id: nil, oauth_app: app, provider_subject: "sub-1", created_by: nil)
    assert bc.valid?, bc.errors.full_messages.to_sentence
  end

  test "created_by is optional" do
    app = build_app
    bc = build_credential(client_id: nil, created_by: nil, oauth_app: app, provider_subject: "sub-2")
    assert bc.valid?, bc.errors.full_messages.to_sentence
  end

  test "effective client credentials come from the columns when standalone" do
    bc = build_credential(client_id: "own-id", client_secret: "own-secret")
    assert_equal "own-id", bc.effective_client_id
    assert_equal "own-secret", bc.effective_client_secret
  end

  test "effective client credentials delegate to the linked app" do
    app = build_app(client_id: "app-cid", client_secret: "app-secret")
    bc = build_credential(client_id: "own-id", client_secret: "own-secret",
                          oauth_app: app, provider_subject: "sub-3", created_by: nil)
    assert_equal "app-cid", bc.effective_client_id
    assert_equal "app-secret", bc.effective_client_secret
  end

  test "external_user_key must be url-safe and bounded" do
    app = build_app
    bc = build_credential(oauth_app: app, provider_subject: "sub-5", created_by: nil, client_id: nil)
    bc.external_user_key = "not safe"
    refute bc.valid?
    bc.external_user_key = "a" * 129
    refute bc.valid?
    bc.external_user_key = "safe-key_123"
    assert bc.valid?, bc.errors.full_messages.to_sentence
  end

  test "client_secret, grant fields, and token_endpoint_headers are encrypted at rest" do
    bc = create_credential(client_secret: "shh", username: "alice", password: "p4ss", api_key: "api-key",
                           token_endpoint_headers: { "X-Api-Key" => "k" })
    raw = BrokerCredential.connection.select_one(
      "SELECT client_secret, username, password, api_key, token_endpoint_headers FROM broker_credentials WHERE id = #{bc.id}"
    )
    refute_includes raw["client_secret"].to_s, "shh"
    refute_includes raw["username"].to_s, "alice"
    refute_includes raw["password"].to_s, "p4ss"
    refute_includes raw["api_key"].to_s, "api-key"
    refute_includes raw["token_endpoint_headers"].to_s, "X-Api-Key"
    assert_equal "alice", bc.reload.username
    assert_equal "p4ss", bc.password
    assert_equal "api-key", bc.api_key
    assert_equal({ "X-Api-Key" => "k" }, bc.reload.token_endpoint_headers)
  end

  test "early_refresh_fraction must be in [0,1)" do
    refute build_credential(early_refresh_fraction: 1.0).valid?
    refute build_credential(early_refresh_fraction: -0.1).valid?
    assert build_credential(early_refresh_fraction: 0.5).valid?
  end

  # --- compute_next_attempt_at ----------------------------------------------

  test "next attempt is now when never refreshed" do
    now = Time.current
    bc = build_credential
    assert_in_delta now.to_f, bc.compute_next_attempt_at(now: now).to_f, 1
  end

  test "next attempt uses the larger of slack and fraction, with a 60s floor" do
    now = Time.current
    # ttl 3600, fraction 0.2 => 720s slack beats the 300s default slack.
    bc = build_credential(early_refresh_slack_seconds: 300, early_refresh_fraction: 0.2)
    bc.last_refresh = now
    bc.expires_at = now + 3600
    assert_in_delta (now + 3600 - 720).to_f, bc.compute_next_attempt_at(now: now).to_f, 1
  end

  test "next attempt is capped by the max refresh interval ceiling" do
    now = Time.current
    bc = build_credential(early_refresh_slack_seconds: 10, early_refresh_fraction: 0.0, max_refresh_interval_seconds: 100)
    bc.last_refresh = now
    bc.expires_at = now + 100_000 # early trigger far in the future
    assert_in_delta (now + 100).to_f, bc.compute_next_attempt_at(now: now).to_f, 1
  end

  # --- delete guard ---------------------------------------------------------

  test "cannot be destroyed while a token_broker source references it" do
    cred = create_credential
    SecretSource.create!(source_type: "token_broker", config: { "credential_id" => cred.oid })
    refute cred.destroy
    assert(cred.errors[:base].any? { |m| m.include?("referenced by") })
    assert BrokerCredential.exists?(cred.id)
  end

  test "can be destroyed once the references are removed" do
    cred = create_credential
    source = SecretSource.create!(source_type: "token_broker", config: { "credential_id" => cred.oid })
    source.destroy!
    assert cred.destroy
  end
end
