require "test_helper"

class BrokerCredentialTest < ActiveSupport::TestCase
  # A stub refresh client returning a fixed Result or raising a fixed error.
  class StubClient
    def initialize(&block) = (@block = block)
    def refresh(**kw) = @block.call(**kw)
  end

  def result(access_token: "AT", refresh_token: "RT", expires_in: 3600)
    Broker::RefreshClient::Result.new(access_token: access_token, refresh_token: refresh_token, expires_in: expires_in)
  end

  def build_credential(refresh_token: "seed-rt", **overrides)
    BrokerCredential.new({
      namespace: "default", foreign_id: "cred-#{SecureRandom.hex(4)}",
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

  test "invalid without a client_id" do
    bc = build_credential(client_id: nil)
    refute bc.valid?
    assert bc.errors[:client_id].any?
  end

  # --- oauth_app provenance (flow-minted credentials) -----------------------

  def build_app(**overrides)
    OauthApp.create!({
      provider: "google", slug: "slug-#{SecureRandom.hex(4)}",
      client_id: "app-cid", client_secret: "app-secret",
      allowed_scopes: %w[a b],
      credential_namespace: "default", created_by: users(:acme_admin)
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

  test "refresh uses the app's client secret for an app-linked credential" do
    captured = {}
    app = build_app(client_id: "app-cid", client_secret: "app-secret")
    bc = create_credential(client_id: nil, client_secret: nil, oauth_app: app,
                           provider_subject: "sub-4", created_by: nil, refresh_token: "rt")
    bc.refresh_client = StubClient.new { |**kw| captured = kw; result }
    bc.refresh!
    assert_equal "app-cid", captured[:client_id]
    assert_equal "app-secret", captured[:client_secret]
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

  test "client_secret and token_endpoint_headers are encrypted at rest" do
    bc = create_credential(client_secret: "shh", token_endpoint_headers: { "X-Api-Key" => "k" })
    raw = BrokerCredential.connection.select_one(
      "SELECT client_secret, token_endpoint_headers FROM broker_credentials WHERE id = #{bc.id}"
    )
    refute_includes raw["client_secret"].to_s, "shh"
    refute_includes raw["token_endpoint_headers"].to_s, "X-Api-Key"
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

  # --- refresh! state machine -----------------------------------------------

  test "successful refresh advances the blob and schedules the next attempt" do
    now = Time.current
    bc = create_credential
    bc.refresh_client = StubClient.new { result(access_token: "AT-1", refresh_token: "RT-2", expires_in: 3600) }
    bc.refresh!(now: now)
    bc.reload
    assert_equal "live", bc.status
    assert_equal "AT-1", bc.access_token
    assert_equal "RT-2", bc.refresh_token
    assert_equal 0, bc.failure_count
    assert_in_delta (now + 3600).to_f, bc.expires_at.to_f, 1
    assert bc.next_attempt_at > now
  end

  test "refresh carries the previous refresh_token forward when the IdP omits it" do
    bc = create_credential(refresh_token: "RT-keep")
    bc.refresh_client = StubClient.new { result(refresh_token: nil, expires_in: nil) }
    bc.refresh!
    bc.reload
    assert_equal "RT-keep", bc.refresh_token
  end

  test "refresh defaults expiry when the IdP omits expires_in" do
    now = Time.current
    bc = create_credential
    bc.refresh_client = StubClient.new { result(expires_in: nil) }
    bc.refresh!(now: now)
    bc.reload
    assert_in_delta (now + BrokerCredential::DEFAULT_EXPIRES_IN_SECONDS).to_f, bc.expires_at.to_f, 1
  end

  test "retryable failure schedules a backoff and does not mark dead" do
    now = Time.current
    bc = create_credential
    bc.refresh_client = StubClient.new { raise Broker::RefreshError.new("net", stage: "network", retryable: true) }
    bc.refresh!(now: now)
    bc.reload
    refute bc.dead?
    assert_equal 1, bc.failure_count
    assert_in_delta (now + BrokerCredential::BACKOFF_BASE_SECONDS).to_f, bc.next_attempt_at.to_f, 1
  end

  test "unrecoverable failure marks the credential dead" do
    bc = create_credential
    bc.refresh_client = StubClient.new { raise Broker::RefreshError.new("bad", stage: "oauth", code: "invalid_grant", retryable: false) }
    bc.refresh!
    bc.reload
    assert bc.dead?
    assert_equal "invalid_grant", bc.dead_reason
  end

  test "refresh passes client credentials and token-endpoint headers to the client" do
    captured = {}
    bc = create_credential(client_id: "the-id", client_secret: "the-secret",
                           token_endpoint_headers: { "X-Api-Key" => "k" })
    bc.refresh_client = StubClient.new { |**kw| captured = kw; result }
    bc.refresh!
    assert_equal "the-id", captured[:client_id]
    assert_equal "the-secret", captured[:client_secret]
    assert_equal({ "X-Api-Key" => "k" }, captured[:headers])
  end

  test "refresh with no seed marks dead as not bootstrapped" do
    bc = create_credential(refresh_token: "seed")
    bc.update_columns(refresh_token: nil)
    bc.reload
    bc.refresh!
    bc.reload
    assert bc.dead?
    assert_equal "blob_not_bootstrapped", bc.dead_reason
  end

  # --- scope ----------------------------------------------------------------

  test "refreshable includes never-attempted and due, excludes dead and future" do
    due = create_credential
    due.update_columns(next_attempt_at: 1.minute.ago)
    future = create_credential
    future.update_columns(next_attempt_at: 1.hour.from_now)
    dead = create_credential
    dead.update_columns(dead: true, next_attempt_at: 1.minute.ago)
    never = create_credential # next_attempt_at nil

    ids = BrokerCredential.refreshable.pluck(:id)
    assert_includes ids, due.id
    assert_includes ids, never.id
    refute_includes ids, future.id
    refute_includes ids, dead.id
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
