ENV["BUNDLE_GEMFILE"] ||= File.expand_path("../Gemfile", __dir__)

require "bundler/setup" # Set up gems listed in the Gemfile.
require "bootsnap/setup" # Speed up boot time by caching expensive operations.

# Plain module for reading CENTAUR_CONSOLE_* env vars (with IRON_CONTROL_*
# fallback). Required this early so config/database.yml ERB and others can use it
# before the app's autoloader is set up.
require_relative "../lib/console_env"
