require "test_helper"
require "tmpdir"
require "open3"
require "rbconfig"
require "active_support/testing/constant_stubbing"

class SshKeyTest < ActiveSupport::TestCase
  include ActiveSupport::Testing::ConstantStubbing
  test "reports a passphrase-protected key without exposing parser input" do
    Dir.mktmpdir do |dir|
      path = File.join(dir, "synthetic")
      _out, _err, status = Open3.capture3("ssh-keygen", "-q", "-t", "ed25519", "-N", "test-only-passphrase", "-f", path)
      assert status.success?
      error = Open3.stub(:popen3, ->(*) { flunk "encrypted keys must not reach OpenSSH" }) do
        assert_raises(SshKey::Invalid) { SshKey.public_key(File.read(path)) }
      end
      assert_includes error.message, "protected by a passphrase"
      assert_not_includes error.message, "test-only-passphrase"
      assert_not_includes error.message, "BEGIN OPENSSH"
    end
  end

  test "unsupported derived types return a readable error before saving" do
    status = Minitest::Mock.new
    status.expect(:success?, true)
    SshKey.stub(:keygen, [ "ssh-dss AAAA", "", status ]) do
      error = assert_raises(SshKey::Invalid) do
        SshKey.public_key("-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----\n")
      end
      assert_includes error.message, "key type is not supported"
    end
    status.verify
    assert_includes SshKey::Invalid.new.message, "invalid or unsupported"
  end

  test "timeout kills and reaps the parser process" do
    original = Open3.method(:popen3)
    pid = nil
    spawn = lambda do |*_arguments, **options, &handler|
      original.call(RbConfig.ruby, "-e", "STDIN.read; sleep 30", **options) do |*streams|
        pid = streams.last.pid
        handler.call(*streams)
      end
    end
    stub_const(SshKey, :KEYGEN_TIMEOUT, 0.05) do
      Open3.stub(:popen3, spawn) do
        error = assert_raises(SshKey::Invalid) { SshKey.send(:keygen, [], "synthetic") }
        assert_includes error.message, "timed out"
      end
    end
    assert pid
    assert_raises(Errno::ESRCH) { Process.kill(0, pid) }
    assert_raises(Errno::ECHILD) { Process.waitpid(pid, Process::WNOHANG) }
  end

  test "reports missing headers incomplete input and malformed key contents" do
    {
      "" => "Paste your private key",
      "copied file contents without headers" => "Include the complete private key",
      "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA" => "key is incomplete",
      "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA\n-----END OPENSSH PRIVATE KEY-----" => "could not read this key"
    }.each do |value, message|
      error = assert_raises(SshKey::Invalid) { SshKey.public_key(value) }
      assert_includes error.message, message
      assert_not_includes error.message, "AAAA"
    end
  end
end
