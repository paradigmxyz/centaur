require "test_helper"

class OauthAppTest < ActiveSupport::TestCase
  def build_app(**overrides)
    OauthApp.new({
      provider: "google", slug: "slug-#{SecureRandom.hex(4)}",
      client_id: "cid", client_secret: "sec",
      allowed_scopes: %w[scope.a scope.b],
      credential_namespace: "default", created_by: users(:acme_admin)
    }.merge(overrides))
  end

  # --- validations ----------------------------------------------------------

  test "valid with all required fields" do
    assert build_app.valid?
  end

  test "provider must be a registered provider" do
    refute build_app(provider: "nope").valid?
    assert build_app(provider: "github").valid?
    assert build_app(provider: "google").valid?
    assert build_app(provider: "slack").valid?
  end

  test "client_id and client_secret are required" do
    refute build_app(client_id: nil).valid?
    refute build_app(client_secret: nil).valid?
  end

  test "credential_namespace must be url-safe" do
    refute build_app(credential_namespace: "not/safe").valid?
  end

  test "slug is required, url-safe, and globally unique" do
    refute build_app(slug: nil).valid?
    refute build_app(slug: "not safe").valid?
    refute build_app(slug: "with/slash").valid?

    build_app(slug: "taken-slug").save!
    dup = build_app(slug: "taken-slug")
    refute dup.valid?
    assert dup.errors[:slug].any?
  end

  test "slug must not shadow the opaque-id prefix" do
    refute build_app(slug: "oap_abc").valid?
  end

  test "allowed_scopes must be a non-empty array of non-blank strings" do
    refute build_app(allowed_scopes: []).valid?
    refute build_app(allowed_scopes: [ "" ]).valid?
    refute build_app(allowed_scopes: "scope.a").valid?
    assert build_app(allowed_scopes: %w[scope.a]).valid?
  end

  test "client_secret is encrypted at rest" do
    app = build_app(client_secret: "shh")
    app.save!
    raw = OauthApp.connection.select_value("SELECT client_secret FROM oauth_apps WHERE id = #{app.id}")
    refute_includes raw.to_s, "shh"
    assert_equal "shh", app.reload.client_secret
  end

  # --- scopes_allowed? ------------------------------------------------------

  test "scopes_allowed? subset check" do
    app = build_app(allowed_scopes: %w[a b c])
    assert app.scopes_allowed?(%w[a b])
    assert app.scopes_allowed?([])
    refute app.scopes_allowed?(%w[a z])
  end

  # --- delete guard ---------------------------------------------------------

  test "cannot be destroyed while it has minted credentials" do
    app = build_app
    app.save!
    BrokerCredential.create!(namespace: "default", foreign_id: "minted-#{SecureRandom.hex(4)}",
                             token_endpoint: "https://oauth2.googleapis.com/token",
                             oauth_app: app, provider_subject: "sub-1")
    refute app.destroy
    assert app.errors[:base].any?
    assert OauthApp.exists?(app.id)
  end
end
