primary_key = ConsoleEnv["AR_ENCRYPTION_PRIMARY_KEY"]
deterministic_key = ConsoleEnv["AR_ENCRYPTION_DETERMINISTIC_KEY"]
key_derivation_salt = ConsoleEnv["AR_ENCRYPTION_KEY_DERIVATION_SALT"]

if Rails.env.production? && ENV["SECRET_KEY_BASE_DUMMY"].blank?
  missing = {
    ConsoleEnv.key("AR_ENCRYPTION_PRIMARY_KEY") => primary_key,
    ConsoleEnv.key("AR_ENCRYPTION_DETERMINISTIC_KEY") => deterministic_key,
    ConsoleEnv.key("AR_ENCRYPTION_KEY_DERIVATION_SALT") => key_derivation_salt
  }.select { |_, v| v.to_s.strip.empty? }.keys

  if missing.any?
    raise "ActiveRecord encryption is not configured: missing env vars #{missing.join(", ")}"
  end
else
  # Dev/test fallback values so the suite and local boot work without env setup.
  primary_key ||= "dev_ar_encryption_primary_key_0000000000000000"
  deterministic_key ||= "dev_ar_encryption_deterministic_key_00000000000"
  key_derivation_salt ||= "dev_ar_encryption_key_derivation_salt_000000000"
end

Rails.application.config.active_record.encryption.primary_key = primary_key
Rails.application.config.active_record.encryption.deterministic_key = deterministic_key
Rails.application.config.active_record.encryption.key_derivation_salt = key_derivation_salt
