require "test_helper"

class SshKeyTest < ActiveSupport::TestCase
  def generate(type, *options, passphrase: "")
    Dir.mktmpdir do |dir|
      path = File.join(dir, "synthetic")
      _out, _err, status = Open3.capture3("ssh-keygen", "-q", "-t", type, *options, "-N", passphrase, "-f", path)
      raise "synthetic key generation failed" unless status.success?

      File.read(path)
    end
  end

  test "derives the public key and fingerprint OpenSSH reports for each supported format" do
    [ %w[ed25519], %w[rsa -b 2048], %w[rsa -b 2048 -m PEM], %w[ecdsa] ].each do |type, *options|
      Dir.mktmpdir do |dir|
        path = File.join(dir, "synthetic")
        _out, _err, status = Open3.capture3("ssh-keygen", "-q", "-t", type, *options, "-N", "", "-f", path)
        assert status.success?
        public_key = File.read("#{path}.pub").split.first(2).join(" ")
        assert_equal public_key, SshKey.public_key(File.read(path).gsub("\n", "\r\n")), type

        expected, = Open3.capture3("ssh-keygen", "-l", "-E", "sha256", "-f", "#{path}.pub")
        assert_equal expected.split[1], SshKey.fingerprint(public_key)
      end
    end
  end

  test "reports passphrase-protected keys without exposing their contents" do
    [ [ "ed25519" ], [ "rsa", "-b", "2048", "-m", "PEM" ] ].each do |type, *options|
      private_key = generate(type, *options, passphrase: "test-only-passphrase")
      error = assert_raises(SshKey::Invalid) { SshKey.public_key(private_key) }
      assert_includes error.message, "protected by a passphrase"
      assert_not_includes error.message, "BEGIN"
    end
  end

  test "reports pasted public keys, missing headers, and malformed contents" do
    {
      "" => "Paste your private key",
      synthetic_ssh_key.last => "This is a public key",
      "copied file contents without headers" => "Include the complete private key",
      "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA" => "Include the complete private key",
      "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----" => "could not be read"
    }.each do |value, message|
      error = assert_raises(SshKey::Invalid) { SshKey.public_key(value) }
      assert_includes error.message, message
      assert_not_includes error.message, "AAAA"
    end
    assert_includes SshKey::Invalid.new.message, "invalid or unsupported"
    assert_raises(SshKey::Invalid) { SshKey.fingerprint("ssh-dss AAAA") }
  end
end
