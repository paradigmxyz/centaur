module Iron
  module Bootstrap
    ADVISORY_LOCK_KEY = 0x1700_B007_C0FFEE & 0x7FFF_FFFF_FFFF_FFFF

    Error = Class.new(StandardError)

    module_function

    def run!(logger: Rails.logger)
      email = ConsoleEnv["INITIAL_USER_EMAIL"].to_s.strip
      return if email.empty?

      password = ConsoleEnv["INITIAL_USER_PASSWORD"].to_s
      if password.empty?
        raise Error, "#{ConsoleEnv.key("INITIAL_USER_EMAIL")} is set but #{ConsoleEnv.key("INITIAL_USER_PASSWORD")} is missing"
      end

      supplied_token = ConsoleEnv["INITIAL_API_KEY"].to_s

      return unless ActiveRecord::Base.connection.data_source_exists?("users")

      ActiveRecord::Base.transaction do
        ActiveRecord::Base.connection.execute("SELECT pg_advisory_xact_lock(#{ADVISORY_LOCK_KEY})")
        return if User.exists?

        # The initial operator predates any approver, so it is created active and
        # admin -- it is the account that approves everyone else.
        user = User.create!(email: email, password: password, status: "active", admin: true)

        api_key = ApiKey.new(user: user, name: "bootstrap")
        unless supplied_token.empty?
          api_key.token = supplied_token
          api_key.token_hash = ApiKey.hash_token(supplied_token)
        end
        api_key.save!

        log_line = "centaur-console bootstrap: created user id=#{user.id} email=#{user.email} api_key_id=#{api_key.id}"
        log_line += " api_key=#{api_key.token}" if supplied_token.empty?
        logger.info(log_line)
      end
    rescue ActiveRecord::RecordInvalid => e
      raise Error, "centaur-console bootstrap failed: #{e.record.class.name.downcase} #{e.record.errors.full_messages.join(", ")}"
    end
  end
end
