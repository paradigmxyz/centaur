require "base64"
require "digest"
require "net/ssh"

# Parses a pasted SSH private key with net-ssh to validate it and derive its
# public key. Error messages never include the submitted key or parser output.
class SshKey
  class Invalid < StandardError
    def initialize(message = "This SSH key is invalid or unsupported. Use an Ed25519, RSA, or ECDSA key.")
      super
    end
  end

  MAX_BYTES = 64.kilobytes
  SUPPORTED_TYPES = %w[ssh-ed25519 ssh-rsa ecdsa-sha2-nistp256 ecdsa-sha2-nistp384 ecdsa-sha2-nistp521].freeze
  PASSPHRASE = "This key is protected by a passphrase. Use a dedicated key without a passphrase.".freeze

  # The "<type> <base64>" public key of +private_key+.
  def self.public_key(private_key)
    raise Invalid, "Paste your private key, including its BEGIN and END lines." if private_key.blank?
    raise Invalid, "The key is too large. Paste a single private key." if private_key.bytesize > MAX_BYTES

    key = normalize(private_key)
    if key.start_with?("ssh-", "ecdsa-", "-----BEGIN PUBLIC KEY-----")
      raise Invalid, "This is a public key. Paste the private key file, without the .pub extension."
    end
    raise Invalid, "Include the complete private key, including its BEGIN and END lines." unless key.start_with?("-----BEGIN ") && key.include?("\n-----END ")
    # Rejected before parsing so OpenSSL never asks for a PEM passphrase.
    raise Invalid, PASSPHRASE if key.include?("BEGIN ENCRYPTED PRIVATE KEY") || key.include?("Proc-Type: 4,ENCRYPTED")

    parsed = Net::SSH::KeyFactory.load_data_private_key(key, nil, false)
    type = parsed.ssh_type
    raise Invalid, "This key type is not supported. Use an Ed25519, RSA, or ECDSA key." unless SUPPORTED_TYPES.include?(type)

    "#{type} #{Base64.strict_encode64(parsed.public_key.to_blob)}"
  rescue Net::SSH::Authentication::ED25519::OpenSSHPrivateKeyLoader::DecryptError
    raise Invalid, PASSPHRASE
  rescue Invalid
    raise
  rescue StandardError, NotImplementedError
    raise Invalid, "This key could not be read. Copy the entire private key file, including both header lines."
  end

  # The OpenSSH-style "SHA256:..." fingerprint of a "<type> <base64>" public key.
  def self.fingerprint(public_key)
    type, body = public_key.to_s.split
    raise Invalid unless SUPPORTED_TYPES.include?(type) && body.present?

    "SHA256:#{Base64.strict_encode64(Digest::SHA256.digest(Base64.strict_decode64(body))).delete("=")}"
  rescue ArgumentError
    raise Invalid
  end

  def self.normalize(private_key)
    private_key.to_s.gsub("\r\n", "\n").gsub("\r", "\n").strip + "\n"
  end
end
