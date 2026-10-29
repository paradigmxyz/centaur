require "test_helper"

class SlackChannelDefaultsTest < ActiveSupport::TestCase
  test "keeps known fields per channel and groups channels by persona" do
    defaults = SlackChannelDefaults.new({
      "C0ENG" => { "persona" => "eng", "harness" => "claude", "model" => "opus", "extra" => "x" },
      "C0OPS" => { "persona" => " ops " },
      "C0LOW" => { "reasoning" => "low" }
    }.to_json)

    assert_equal({ "persona" => "eng", "harness" => "claude", "model" => "opus" }, defaults.for_channel("C0ENG"))
    assert_equal "ops", defaults.persona_for_channel("C0OPS")
    assert_nil defaults.persona_for_channel("C0LOW")
    assert_equal %w[C0ENG], defaults.channels_for_persona("eng").keys
  end

  test "skips malformed entries and tolerates bad or missing JSON" do
    defaults = SlackChannelDefaults.new({ "C0BAD" => "eng", " " => { "persona" => "eng" }, "C0EMPTY" => {} }.to_json)
    assert_empty defaults.entries

    assert_empty SlackChannelDefaults.new("{not json").entries
    assert_empty SlackChannelDefaults.new("[]").entries
    assert_empty SlackChannelDefaults.new(nil).entries
  end

  test "reads the console-prefixed environment variable" do
    with_env("CENTAUR_CONSOLE_SLACK_CHANNEL_DEFAULTS" => { "C0ENG" => { "persona" => "eng" } }.to_json) do
      assert_equal "eng", SlackChannelDefaults.current.persona_for_channel("C0ENG")
    end
  end
end
