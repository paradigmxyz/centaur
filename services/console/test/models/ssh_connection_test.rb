require "test_helper"
require "tmpdir"
require "open3"


class SshConnectionTest < ActiveSupport::TestCase
  def self.synthetic_key
    @synthetic_key ||= Dir.mktmpdir do |dir|
      path = File.join(dir, "synthetic")
      _out, _err, status = Open3.capture3("ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", path)
      raise "synthetic key generation failed" unless status.success?
      [ File.read(path), File.read("#{path}.pub").split.first(2).join(" ") ]
    end
  end

  def connection
    private_key, = self.class.synthetic_key
    users(:member_user).build_ssh_connection(name: "My SSH key", private_key: private_key)
  end

  test "validates and encrypts a personal key and derives its public key" do
    record = connection
    assert record.save, record.errors.full_messages.join(", ")
    assert_equal self.class.synthetic_key.last, record.public_key
    assert record.configured?
    assert_match(/\ASHA256:/, record.key_fingerprint)
    stored = record.attributes_before_type_cast["private_key"]
    assert_match(/\A\{/, stored)
    assert_no_match(/BEGIN OPENSSH/, stored)
    assert_nil record.expires_at
  end

  test "normalizes browser CRLF and a missing final newline before encryption" do
    record = connection
    record.private_key = record.private_key.strip.gsub("\n", "\r\n")
    record.save!
    assert_equal self.class.synthetic_key.last, record.public_key
    assert_equal self.class.synthetic_key.first, record.reload.private_key
  end

  test "rejects invalid private keys without exposing their value" do
    record = connection
    record.private_key = "invalid synthetic key"
    assert_not record.valid?
    assert record.errors[:private_key].any?
    assert_not_includes record.errors.full_messages.join, "invalid synthetic key"
  end

  test "key rotation and revocation change the lease generation and remove the key" do
    record = connection
    record.save!
    generation = record.generation
    record.update!(name: "Other EE")
    assert_not_equal generation, record.generation
    generation = record.generation
    record.revoke!
    assert record.revoked?
    assert_not record.configured?
    assert_nil record.reload.private_key
    assert_not_equal generation, record.generation
  end

  test "keys stay connected without expiry and a second connection for a user is refused" do
    record = connection
    record.save!
    duplicate = record.dup
    assert_not duplicate.valid?
    travel 1.year do
      assert record.reload.configured?
    end
    record.update_columns(expires_at: 1.minute.ago)
    assert_not record.configured?
  end
end
