require "digest"

# Lookup of single-use secrets stored as SHA-256 digests. Including models
# declare the digest column and must define a `usable` scope.
module HashedTokenLookup
  extend ActiveSupport::Concern

  class_methods do
    # Acts as both setter (when called with a value) and getter.
    #   class McpOauthRefreshToken < ApplicationRecord
    #     token_hash_attribute :token_hash
    #   end
    def token_hash_attribute(value = nil)
      @token_hash_attribute = value.to_sym if value
      @token_hash_attribute or
        raise NotImplementedError, "#{name} must declare `token_hash_attribute :...`"
    end

    def hash_token(value)
      Digest::SHA256.hexdigest(value.to_s)
    end

    def find_usable(value)
      usable.find_by(token_hash_attribute => hash_token(value))
    end
  end
end
