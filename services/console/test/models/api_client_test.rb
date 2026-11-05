require "test_helper"

class ApiClientTest < ActiveSupport::TestCase
  def create_client(overrides = {})
    ApiClient.create!({ name: "eval-runner", scopes: ApiClient::SCOPES, created_by: users(:acme_admin) }.merge(overrides))
  end

  test "issues a secret once and stores only its digest" do
    client = create_client

    assert_match(/\Aacs_[0-9a-f]{64}\z/, client.secret)
    assert_equal Digest::SHA256.hexdigest(client.secret), client.secret_digest
    assert_nil ApiClient.find(client.id).secret
  end

  test "rotating replaces the secret digest" do
    client = create_client
    original = client.secret_digest

    client.rotate_secret!

    assert_not_equal original, client.reload.secret_digest
    assert_equal ApiClient.digest_secret(client.secret), client.secret_digest
  end

  test "requires known, non-empty scopes" do
    client = ApiClient.new(name: "eval-runner", created_by: users(:acme_admin), scopes: [])
    assert_not client.valid?
    assert_includes client.errors[:scopes], "must include at least one scope"

    client.scopes = %w[sessions:read admin]
    assert_not client.valid?
    assert_includes client.errors[:scopes], "contains unsupported values: admin"
  end

  test "the first attached principal becomes the default and make_default moves it" do
    client = create_client
    alice = client.attach_principal!(principals(:acme_user_alice))
    bob = client.attach_principal!(principals(:acme_user_bob))

    assert alice.reload.default
    assert_not bob.reload.default

    client.make_default!(bob)

    assert_not alice.reload.default
    assert bob.reload.default
    assert_equal bob, client.default_principal_link
  end

  test "attaching the same principal twice keeps one link" do
    client = create_client
    client.attach_principal!(principals(:acme_channel))
    client.attach_principal!(principals(:acme_channel))

    assert_equal 1, client.api_client_principals.count
  end

  test "deleting a principal detaches it" do
    client = create_client
    principal = principals(:acme_user_bob)
    client.attach_principal!(principal)

    principal.destroy!

    assert_empty client.api_client_principals.reload
  end
end
