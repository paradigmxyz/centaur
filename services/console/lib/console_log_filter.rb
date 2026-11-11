require "uri"
require "active_support/parameter_filter"

# Log filtering for parameter names that reach Rails' filters undecoded.
#
# Rails filters the "Parameters" log line with decoded names, but the
# "Started GET /path?..." line (request.filtered_path) and the
# "Redirected to ..." line (response.filtered_location) filter the raw query
# string pair by pair, with the name still percent-encoded. So
# "?c%6fde=secret" (which Rails parses as "code") or "?data[code]=secret"
# slip past name filters such as /\Acode\z/.
#
# decoded_name_filter returns a filter_parameters block that percent-decodes
# the name (as Rack does, repeatedly in case of double encoding), splits
# nested names ("data[code]", "a.b") into segments, and masks the value when
# the decoded name or any segment matches the base filters. It only rewrites
# the copy Rails logs; request parameters are unchanged.
module ConsoleLogFilter
  MASK = "[FILTERED]".freeze
  MAX_DECODE_ROUNDS = 3

  module_function

  def decoded_name_filter(base_filters)
    inner = ActiveSupport::ParameterFilter.new(base_filters)
    lambda do |key, value|
      next unless value.is_a?(String) && !value.frozen?
      next unless filtered_name?(inner, key.to_s)

      value.replace(MASK)
    end
  end

  def filtered_name?(inner, raw)
    names = decoded_names(raw)
    names.any? { |name| inner.filter(name => "")[name] == MASK }
  end

  # The decoded name plus its nested segments.
  def decoded_names(raw)
    decoded = raw
    MAX_DECODE_ROUNDS.times do
      next_value = URI.decode_www_form_component(decoded)
      break if next_value == decoded

      decoded = next_value
    rescue ArgumentError
      break
    end
    ([ decoded ] + decoded.split(/[\[\].]+/)).reject(&:empty?).uniq
  end
end
