require "uri"

module Login
  # Fetches and caches the small public documents needed to validate an OIDC
  # login. Issuers are operator configuration, but all discovered URLs are still
  # constrained to HTTPS and the issuer's origin to avoid turning login into a
  # general-purpose HTTP client.
  module OidcDiscovery
    CACHE_TTL = 1.hour
    MAX_DOCUMENT_BYTES = 256.kilobytes

    module_function

    def metadata(issuer)
      normalized = normalized_issuer(issuer)
      cache("metadata", normalized) do
        document = fetch_json("#{normalized}/.well-known/openid-configuration")
        unless document["issuer"] == normalized
          raise exchange_error("OIDC discovery issuer did not match configured issuer", "oidc_issuer_mismatch")
        end

        %w[authorization_endpoint token_endpoint userinfo_endpoint jwks_uri].each do |field|
          validate_endpoint!(document[field], issuer: normalized)
        end
        document
      end
    end

    def jwks(metadata, force: false)
      uri = metadata.fetch("jwks_uri")
      cache("jwks", uri, force: force) { fetch_json(uri) }
    end

    def normalized_issuer(value)
      uri = URI.parse(value.to_s.strip.delete_suffix("/"))
      unless uri.is_a?(URI::HTTPS) && uri.host.present? && uri.userinfo.nil? && uri.query.nil? && uri.fragment.nil?
        raise exchange_error("OIDC issuer must be an HTTPS URL", "oidc_issuer_invalid")
      end
      uri.to_s
    rescue URI::InvalidURIError
      raise exchange_error("OIDC issuer must be an HTTPS URL", "oidc_issuer_invalid")
    end

    def fetch_json(url)
      response = begin
        HttpClient.new(max_body_bytes: MAX_DOCUMENT_BYTES).get(url)
      rescue StandardError
        raise exchange_error("OIDC document request failed", "oidc_document_failed")
      end
      unless response.success?
        raise exchange_error("OIDC document request failed", "oidc_document_failed")
      end
      document = response.json
      unless document.is_a?(Hash)
        raise exchange_error("OIDC document was not a JSON object", "oidc_document_invalid")
      end
      document
    rescue JSON::ParserError
      raise exchange_error("OIDC document was not valid JSON", "oidc_document_invalid")
    end

    def validate_endpoint!(value, issuer:)
      valid = begin
        endpoint = URI.parse(value.to_s)
        origin = URI.parse(issuer)
        endpoint.is_a?(URI::HTTPS) && endpoint.host == origin.host && endpoint.port == origin.port &&
          endpoint.userinfo.nil? && endpoint.fragment.nil?
      rescue URI::InvalidURIError
        false
      end
      return if valid

      raise exchange_error("OIDC discovery returned an invalid endpoint", "oidc_endpoint_invalid")
    end

    def cache(kind, source, force: false, &)
      Rails.cache.fetch("console_oidc/#{kind}/#{source}", expires_in: CACHE_TTL, force: force, &)
    end

    def exchange_error(message, code)
      Broker::ExchangeError.new(message, stage: "oauth", code: code)
    end
  end
end
