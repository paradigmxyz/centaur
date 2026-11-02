require "open3"
require "base64"
require "timeout"

class SshKey
  class Invalid < StandardError
    def initialize(message = "This SSH key is invalid or unsupported. Use an Ed25519, RSA, or ECDSA key.")
      super
    end
  end

  KEYGEN_TIMEOUT = 5
  SUPPORTED_TYPES = %w[ssh-ed25519 ssh-rsa ecdsa-sha2-nistp256 ecdsa-sha2-nistp384 ecdsa-sha2-nistp521].freeze

  def self.public_key(private_key)
    raise Invalid, "Paste your private key, including its BEGIN and END lines." if private_key.blank?
    raise Invalid, "The key is too large. Paste a single private key." if private_key.bytesize > 64.kilobytes

    key = normalize(private_key)
    if key.start_with?("ssh-", "ecdsa-", "-----BEGIN PUBLIC KEY-----")
      raise Invalid, "This is a public key. Paste the private key file, without the .pub extension."
    end
    unless key.match?(/\A-----BEGIN (?:OPENSSH |RSA |EC |DSA |ENCRYPTED )?PRIVATE KEY-----\n/)
      raise Invalid, "Include the complete private key, starting with its BEGIN PRIVATE KEY or BEGIN OPENSSH PRIVATE KEY line."
    end
    unless key.match?(/-----END (?:OPENSSH |RSA |EC |DSA |ENCRYPTED )?PRIVATE KEY-----\n\z/)
      raise Invalid, "The key is incomplete. Include its END PRIVATE KEY or END OPENSSH PRIVATE KEY line."
    end
    raise Invalid, "This key is protected by a passphrase. Use a dedicated key without a passphrase." if encrypted?(key)

    # Stdin keeps the private key out of argv and plaintext files. Never expose
    # ssh-keygen's stderr: parse failures may include input-derived text.
    output, error, status = keygen([ "-y", "-P", "", "-f", "/dev/stdin" ], key)
    unless status.success?
      if error.downcase.include?("passphrase")
        raise Invalid, "This key is protected by a passphrase. Use a dedicated key without a passphrase."
      end
      raise Invalid, "OpenSSH could not read this key. Copy the entire private key file, including both header lines and all contents."
    end

    parts = output.split.first(2)
    raise Invalid, "This key type is not supported. Use an Ed25519, RSA, or ECDSA key." unless SUPPORTED_TYPES.include?(parts.first)

    parts.join(" ")
  rescue Errno::ENOENT
    raise Invalid, "Key validation is temporarily unavailable. Try again later."
  end

  def self.keygen(arguments, input)
    Open3.popen3("ssh-keygen", *arguments, pgroup: true) do |stdin, stdout, stderr, process|
      readers = []
      begin
        Timeout.timeout(KEYGEN_TIMEOUT) do
          readers = [ Thread.new { stdout.read }, Thread.new { stderr.read } ]
          stdin.write(input)
          stdin.close
          [ readers[0].value, readers[1].value, process.value ]
        end
      rescue Timeout::Error
        raise Invalid, "Key validation timed out. Try another key."
      rescue Errno::EPIPE
        raise Invalid, "OpenSSH could not read this key. Copy the complete private key file."
      ensure
        if process.alive?
          begin
            Process.kill("KILL", -process.pid)
          rescue Errno::ESRCH
            # The process exited between the liveness check and the signal.
          end
        end
        stdin.close unless stdin.closed?
        process.value
        readers.each(&:join)
      end
    end
  end
  private_class_method :keygen

  def self.normalize(private_key)
    private_key.to_s.gsub("\r\n", "\n").gsub("\r", "\n").strip + "\n"
  end

  def self.encrypted?(key)
    return true if key.include?("BEGIN ENCRYPTED PRIVATE KEY") || key.include?("Proc-Type: 4,ENCRYPTED")
    return false unless key.start_with?("-----BEGIN OPENSSH PRIVATE KEY-----")

    wire = Base64.strict_decode64(key.lines[1...-1].join.delete("\n"))
    magic = "openssh-key-v1\0"
    return false unless wire.start_with?(magic) && wire.bytesize >= magic.bytesize + 4

    length = wire.byteslice(magic.bytesize, 4).unpack1("N")
    cipher = wire.byteslice(magic.bytesize + 4, length)
    cipher.present? && cipher.bytesize == length && cipher != "none"
  rescue ArgumentError
    false
  end
  private_class_method :encrypted?

  def self.fingerprint(public_key)
    raise Invalid if public_key.to_s.bytesize > 16.kilobytes

    parts = public_key.to_s.split
    raise Invalid unless parts.size >= 2 && SUPPORTED_TYPES.include?(parts[0])

    output, _error, status = keygen([ "-l", "-E", "sha256", "-f", "/dev/stdin" ], parts.first(2).join(" "))
    raise Invalid unless status.success?

    output.split[1]
  rescue ArgumentError, Errno::ENOENT
    raise Invalid
  end
end
