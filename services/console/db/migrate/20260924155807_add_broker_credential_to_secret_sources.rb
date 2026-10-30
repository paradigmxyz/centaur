require "digest"
require "sqids"

class AddBrokerCredentialToSecretSources < ActiveRecord::Migration[8.1]
  class MigrationSecretSource < ActiveRecord::Base
    self.table_name = "secret_sources"
  end

  class MigrationBrokerCredential < ActiveRecord::Base
    self.table_name = "broker_credentials"
  end

  CHECK_NAME = "secret_sources_broker_credential_requires_token_broker".freeze
  OID_PREFIX = "bcr".freeze
  OID_ALPHABET = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".freeze
  OID_MIN_LENGTH = 8

  def up
    add_reference :secret_sources, :broker_credential, null: true, foreign_key: true
    MigrationSecretSource.reset_column_information

    unresolved_source_ids = []
    MigrationSecretSource.where(source_type: "token_broker").find_each do |source|
      reference = source.config.to_h["credential_id"]
      credential = resolve_credential(reference)
      unless credential
        unresolved_source_ids << source.id
        next
      end

      source.update_columns(broker_credential_id: credential.id)
    end
    if unresolved_source_ids.any?
      say "Left unresolved token_broker secret sources unchanged: #{unresolved_source_ids.join(", ")}"
    end

    add_check_constraint :secret_sources,
                         "broker_credential_id IS NULL OR source_type = 'token_broker'",
                         name: CHECK_NAME
  end

  def down
    remove_check_constraint :secret_sources, name: CHECK_NAME
    remove_reference :secret_sources, :broker_credential, foreign_key: true
  end

  private

  def resolve_credential(reference)
    return nil if reference.blank?

    id = decode_oid(reference)
    return MigrationBrokerCredential.find_by(id: id) if id

    MigrationBrokerCredential.find_by(foreign_id: reference)
  end

  def decode_oid(value)
    prefix, separator, encoded = value.to_s.partition("_")
    return nil if separator.empty? || encoded.empty? || prefix != OID_PREFIX

    encoder = Sqids.new(alphabet: shuffled_alphabet, min_length: OID_MIN_LENGTH)
    decoded = encoder.decode(encoded)
    return nil unless decoded.length == 1 && encoder.encode(decoded) == encoded

    decoded.first
  rescue ArgumentError
    nil
  end

  def shuffled_alphabet
    @shuffled_alphabet ||= OID_ALPHABET.chars.sort_by do |character|
      Digest::SHA256.hexdigest("#{OID_PREFIX}:#{character}")
    end.join
  end
end
