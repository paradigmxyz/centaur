require "test_helper"

class SshKeySecretTest < ActiveSupport::TestCase
  def personal_key(user = users(:member_user))
    private_key, = synthetic_ssh_key
    user.build_personal_ssh_key(name: "My SSH key", created_by: user, private_key: private_key)
  end

  test "stores an inline key encrypted in its secret source and derives the public key" do
    key = personal_key
    assert key.save, key.errors.full_messages.join(", ")
    assert_equal synthetic_ssh_key.last, key.public_key
    assert_match(/\ASHA256:/, key.key_fingerprint)
    assert key.personal?
    source = key.key_source
    assert_equal "control_plane", source.source_type
    stored = source.attributes_before_type_cast["secret"]
    assert_match(/\A\{/, stored)
    assert_no_match(/BEGIN OPENSSH/, stored)
  end

  test "normalizes browser CRLF and a missing final newline before encryption" do
    key = personal_key
    key.private_key = synthetic_ssh_key.first.strip.gsub("\n", "\r\n")
    key.save!
    assert_equal synthetic_ssh_key.last, key.public_key
    assert_equal synthetic_ssh_key.first, key.reload.key_source.secret
  end

  test "rejects invalid or missing private keys without exposing their value" do
    key = personal_key
    key.private_key = "invalid synthetic key"
    assert_not key.valid?
    assert key.errors[:private_key].any?
    assert_not_includes key.errors.full_messages.join, "invalid synthetic key"

    missing = SshKeySecret.new(name: "No key", created_by: users(:acme_admin))
    assert_not missing.valid?
    assert_includes missing.errors[:private_key], "can't be blank"
  end

  test "an externally sourced key needs a supported public key" do
    key = SshKeySecret.new(name: "Team key", created_by: users(:acme_admin),
                           key_source: SecretSource.new(source_type: "env", config: { "var" => "TEAM_SSH_KEY" }))
    assert_not key.valid?
    assert key.errors[:public_key].any?
    key.public_key = "#{synthetic_ssh_key.last} team@example"
    assert key.save, key.errors.full_messages.join(", ")
    assert_equal synthetic_ssh_key.last, key.public_key
  end

  test "replacing the private key touches the secret so proxy leases change" do
    key = personal_key
    key.save!
    before = key.updated_at
    replacement, replacement_public = SyntheticSshKey.generate
    travel 1.minute do
      key.update!(private_key: replacement)
      assert_operator key.reload.updated_at, :>, before
    end
    assert_equal replacement_public, key.public_key
    assert_equal 1, SecretSource.where(ssh_key_secret_id: key.id).count
  end

  test "a user has at most one personal key and deleting it removes its grants" do
    key = personal_key
    key.save!
    duplicate = SshKeySecret.new(name: "Second", owner: users(:member_user), created_by: users(:member_user),
                                 private_key: synthetic_ssh_key.first)
    assert_not duplicate.valid?
    Grant.create!(principal: principals(:acme_channel), ssh_key_secret: key, created_by: users(:acme_admin))
    assert_difference -> { Grant.where(ssh_key_secret_id: key.id).count }, -1 do
      key.destroy!
    end
    assert_nil SecretSource.find_by(ssh_key_secret_id: key.id)
  end
end
