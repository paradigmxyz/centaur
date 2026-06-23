# Reads process configuration from the environment under the console's variable
# prefix.
#
# The console was originally "iron-control", and all of its environment
# variables were prefixed IRON_CONTROL_. The project has since been renamed to
# centaur-console, so the canonical prefix is now CENTAUR_CONSOLE_. To avoid
# breaking existing deployments, every lookup falls back to the legacy
# IRON_CONTROL_ name when the CENTAUR_CONSOLE_ one is unset.
#
# Call sites pass the *suffix* -- the part after the prefix:
#   ConsoleEnv["PUBLIC_URL"]          # ENV["CENTAUR_CONSOLE_PUBLIC_URL"]
#                                     #   || ENV["IRON_CONTROL_PUBLIC_URL"]
#   ConsoleEnv.fetch("DB_PORT", 5432) # with a default
#   ConsoleEnv.key("PUBLIC_URL")      # "CENTAUR_CONSOLE_PUBLIC_URL" (for messages)
#
# This is a plain module (no Rails dependencies) so it can be required from
# config/boot.rb and used in early-boot ERB such as config/database.yml.
module ConsoleEnv
  PREFIX = "CENTAUR_CONSOLE".freeze
  LEGACY_PREFIX = "IRON_CONTROL".freeze

  module_function

  # Canonical (new) variable name for a suffix, e.g. "PUBLIC_URL" ->
  # "CENTAUR_CONSOLE_PUBLIC_URL". Use in error messages so operators are pointed
  # at the name they should set going forward.
  def key(suffix) = "#{PREFIX}_#{suffix}"

  # Legacy variable name, still honored for backwards compatibility.
  def legacy_key(suffix) = "#{LEGACY_PREFIX}_#{suffix}"

  # Value of the canonical variable, or the legacy one, or nil. A canonical
  # variable that is explicitly set to an empty string wins over a legacy one,
  # matching plain ENV semantics; call sites use .presence where empty means
  # "unset".
  def [](suffix)
    value = ENV[key(suffix)]
    return value unless value.nil?
    ENV[legacy_key(suffix)]
  end

  # Hash#fetch over both names: returns the value if either is set, otherwise the
  # default, or yields the canonical key, or raises KeyError.
  def fetch(suffix, *default)
    value = self[suffix]
    return value unless value.nil?

    if block_given?
      yield key(suffix)
    elsif !default.empty?
      default.first
    else
      raise KeyError, "neither #{key(suffix)} nor #{legacy_key(suffix)} is set"
    end
  end
end
