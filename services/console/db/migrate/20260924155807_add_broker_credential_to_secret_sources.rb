require "digest"
require "sqids"

class AddBrokerCredentialToSecretSources < ActiveRecord::Migration[8.1]
  class MigrationSecretSource < ActiveRecord::Base
    self.table_name = "secret_sources"
  end

  class MigrationBrokerCredential < ActiveRecord::Base
    self.table_name = "broker_credentials"
  end

  CHECK_NAME = "secret_sources_token_broker_credential_present".freeze
  OID_PREFIX = "bcr".freeze
  OID_ALPHABET = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".freeze
  OID_MIN_LENGTH = 8

  def up
    add_reference :secret_sources, :broker_credential, null: true, foreign_key: true
    MigrationSecretSource.reset_column_information

    MigrationSecretSource.where(source_type: "token_broker").find_each do |source|
      reference = source.config.to_h["credential_id"]
      credential = resolve_credential(reference)
      unless credential
        raise ActiveRecord::MigrationError,
              "token_broker secret source #{source.id} references missing broker credential #{reference.inspect}"
      end

      source.update_columns(
        broker_credential_id: credential.id,
        config: source.config.to_h.except("credential_id")
      )
    end

    add_check_constraint :secret_sources,
                         "(source_type = 'token_broker') = (broker_credential_id IS NOT NULL)",
                         name: CHECK_NAME
  end

  def down
    MigrationSecretSource.reset_column_information
    MigrationSecretSource.where(source_type: "token_broker").find_each do |source|
      reference = encode_oid(source.broker_credential_id)
      source.update_columns(config: source.config.to_h.merge("credential_id" => reference))
    end

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

    decoded = oid_encoder.decode(encoded)
    return nil unless decoded.length == 1 && oid_encoder.encode(decoded) == encoded

    decoded.first
  rescue ArgumentError
    nil
  end

  def encode_oid(id)
    "#{OID_PREFIX}_#{oid_encoder.encode([ id ])}"
  end

  def oid_encoder
    @oid_encoder ||= Sqids.new(alphabet: shuffled_alphabet, min_length: OID_MIN_LENGTH)
  end

  def shuffled_alphabet
    @shuffled_alphabet ||= OID_ALPHABET.chars.sort_by do |character|
      Digest::SHA256.hexdigest("#{OID_PREFIX}:#{character}")
    end.join
  end
end
