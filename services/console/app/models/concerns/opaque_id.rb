module OpaqueId
  extend ActiveSupport::Concern

  BASE_ALPHABET = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".freeze
  MIN_LENGTH = 8

  class_methods do
    # Acts as both setter (when called with a value) and getter.
    #   class Principal < ApplicationRecord
    #     oid_prefix "prn"
    #   end
    def oid_prefix(value = nil)
      if value
        prefix = value.to_s.freeze
        raise ArgumentError, "#{name}: oid prefix must not be blank" if prefix.empty?
        raise ArgumentError, "#{name}: oid prefix must not contain underscores" if prefix.include?("_")

        @oid_prefix = prefix
        @oid_encoder = nil
      end

      @oid_prefix or raise NotImplementedError, "#{name} must declare `oid_prefix \"...\"`"
    end

    def oid_encoder
      @oid_encoder ||= Sqids.new(
        alphabet: OpaqueId.shuffled_alphabet(oid_prefix),
        min_length: MIN_LENGTH
      )
    end

    def decode_oid(value)
      return nil if value.blank?
      prefix, separator, encoded = value.to_s.partition("_")
      return nil if separator.empty? || encoded.empty?
      return nil unless prefix == oid_prefix

      decoded = oid_encoder.decode(encoded)
      return nil unless decoded.length == 1

      id = decoded.first
      # Round-trip guard: Sqids accepts some non-canonical strings, and can decode
      # malformed input to numbers outside its encodable range (which makes encode
      # raise ArgumentError). Either way, treat as not-a-real-oid.
      return nil unless oid_encoder.encode([ id ]) == encoded

      id
    rescue ArgumentError
      nil
    end

    def find_by_oid(value)
      id = decode_oid(value)
      id && find_by(id: id)
    end

    def find_by_oid!(value)
      find_by_oid(value) or
        raise ActiveRecord::RecordNotFound, "Couldn't find #{name} with oid #{value.inspect}"
    end
  end

  def oid
    return nil unless id
    "#{self.class.oid_prefix}_#{self.class.oid_encoder.encode([ id ])}"
  end

  # Address records by their opaque id in generated URLs (form_with model:,
  # polymorphic_path, *_path(record)), matching the find_by_oid! lookups the
  # controllers use. Returns nil for an unsaved record so form_with still routes
  # it to the create action.
  def to_param
    oid
  end

  # Deterministically permutes the base alphabet using the prefix as a seed,
  # so each model gets a distinct encoding space. An id encoded under one
  # prefix will not decode to the same id under another.
  def self.shuffled_alphabet(seed)
    BASE_ALPHABET.chars.sort_by { |c| Digest::SHA256.hexdigest("#{seed}:#{c}") }.join
  end
end
