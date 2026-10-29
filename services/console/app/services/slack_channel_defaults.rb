# Per-channel Slack defaults (persona, harness, model, provider, reasoning),
# parsed from the JSON map Slackbot reads as SLACKBOTV2_CHANNEL_DEFAULTS. The
# chart passes the same value to the console as
# CENTAUR_CONSOLE_SLACK_CHANNEL_DEFAULTS. Read-only: Slackbot stays the owner.
class SlackChannelDefaults
  FIELDS = %w[persona harness model provider reasoning].freeze

  def self.current
    new(ConsoleEnv["SLACK_CHANNEL_DEFAULTS"])
  end

  # Channel id => present fields, e.g. { "C0ENG" => { "persona" => "eng" } }.
  attr_reader :entries

  def initialize(raw)
    @entries = parse(raw)
  end

  def for_channel(channel_id)
    entries[channel_id.to_s]
  end

  def persona_for_channel(channel_id)
    for_channel(channel_id)&.dig("persona")
  end

  def channels_for_persona(persona_id)
    entries.select { |_channel_id, fields| fields["persona"] == persona_id.to_s }
  end

  private

  def parse(raw)
    return {} if raw.blank?

    parsed = JSON.parse(raw)
    return {} unless parsed.is_a?(Hash)

    parsed.each_with_object({}) do |(channel_id, fields), result|
      key = channel_id.to_s.strip
      next if key.blank? || !fields.is_a?(Hash)

      present = fields.slice(*FIELDS).transform_values { |value| value.to_s.strip }.compact_blank
      result[key] = present if present.any?
    end
  rescue JSON::ParserError => e
    Rails.logger.warn("console_slack_channel_defaults_invalid error=#{e.class}: #{e.message}")
    {}
  end
end
