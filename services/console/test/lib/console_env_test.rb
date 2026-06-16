require "test_helper"

class ConsoleEnvTest < ActiveSupport::TestCase
  KEYS = %w[CENTAUR_CONSOLE_WIDGET IRON_CONTROL_WIDGET].freeze

  setup do
    @prev_env = ENV.to_hash.slice(*KEYS)
    KEYS.each { |k| ENV.delete(k) }
  end

  teardown do
    KEYS.each { |k| ENV.delete(k) }
    @prev_env.each { |k, v| ENV[k] = v }
  end

  test "[] reads the canonical CENTAUR_CONSOLE_ variable" do
    ENV["CENTAUR_CONSOLE_WIDGET"] = "new"
    assert_equal "new", ConsoleEnv["WIDGET"]
  end

  test "[] falls back to the legacy IRON_CONTROL_ variable" do
    ENV["IRON_CONTROL_WIDGET"] = "legacy"
    assert_equal "legacy", ConsoleEnv["WIDGET"]
  end

  test "[] prefers the canonical variable over the legacy one" do
    ENV["CENTAUR_CONSOLE_WIDGET"] = "new"
    ENV["IRON_CONTROL_WIDGET"] = "legacy"
    assert_equal "new", ConsoleEnv["WIDGET"]
  end

  test "[] returns nil when neither is set" do
    assert_nil ConsoleEnv["WIDGET"]
  end

  test "fetch returns the default when neither is set" do
    assert_equal 5432, ConsoleEnv.fetch("WIDGET", 5432)
  end

  test "fetch returns the value (including legacy) over the default" do
    ENV["IRON_CONTROL_WIDGET"] = "legacy"
    assert_equal "legacy", ConsoleEnv.fetch("WIDGET", "default")
  end

  test "fetch yields the canonical key when unset and no default given" do
    yielded = nil
    ConsoleEnv.fetch("WIDGET") { |k| yielded = k }
    assert_equal "CENTAUR_CONSOLE_WIDGET", yielded
  end

  test "fetch raises KeyError when unset with no default or block" do
    assert_raises(KeyError) { ConsoleEnv.fetch("WIDGET") }
  end

  test "key and legacy_key build the prefixed names" do
    assert_equal "CENTAUR_CONSOLE_WIDGET", ConsoleEnv.key("WIDGET")
    assert_equal "IRON_CONTROL_WIDGET", ConsoleEnv.legacy_key("WIDGET")
  end
end
