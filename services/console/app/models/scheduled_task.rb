require "fugit"

class ScheduledTask < ApplicationRecord
  class DeliveryDestinationUnavailable < StandardError; end

  oid_prefix "tsk"

  WORKFLOW_NAME = "console_workflow".freeze
  DEFAULT_TIMEZONE = "America/Los_Angeles".freeze
  SCHEDULE_PRESETS = {
    "daily" => "0 9 * * *",
    "weekdays" => "0 9 * * 1-5",
    "mondays" => "0 9 * * 1",
    "fridays" => "0 9 * * 5"
  }.freeze
  SCHEDULE_PRESET_LABELS = {
    "manual" => "Manual only",
    "daily" => "Every day",
    "weekdays" => "Every weekday",
    "mondays" => "Mondays",
    "fridays" => "Fridays"
  }.freeze
  SCHEDULE_LABELS = {
    "daily" => "Daily at 9:00 PT",
    "weekdays" => "Weekdays at 9:00 PT",
    "mondays" => "Mondays at 9:00 PT",
    "fridays" => "Fridays at 9:00 PT"
  }.freeze
  CUSTOM_SCHEDULE_DAYS = [
    [ "Mon", "1" ],
    [ "Tue", "2" ],
    [ "Wed", "3" ],
    [ "Thu", "4" ],
    [ "Fri", "5" ],
    [ "Sat", "6" ],
    [ "Sun", "0" ]
  ].freeze
  DELIVERY_DESTINATION_FORMAT = /\A[CDGUW][A-Z0-9]{8,}\z/

  # A task is owned by exactly one of a Console user (author) or a Slack channel
  # principal. Channel tasks run with the channel's permissions and always
  # deliver to that channel.
  belongs_to :author, class_name: "User", optional: true
  belongs_to :principal, optional: true

  scope :due, ->(now = Time.current) { where(enabled: true, next_run_at: ..now) }

  normalizes :name, :delivery_channel, :timezone, with: ->(value) { value.to_s.strip }
  normalizes :cron_expression, with: ->(value) { value.to_s.strip.presence }
  normalizes :prompt, with: ->(value) { value.to_s.gsub("\r\n", "\n").strip }

  before_validation :set_default_timezone
  before_validation :set_channel_delivery, if: :principal
  before_validation :refresh_next_run_at, if: :schedule_requires_refresh?

  validates :name, presence: true, length: { maximum: 100 },
                   uniqueness: { scope: :author_id, if: :author_id? }
  validates :name, uniqueness: { scope: :principal_id, if: :principal_id? }
  validates :prompt, presence: true, length: { maximum: 64.kilobytes }
  validates :delivery_channel, presence: true,
                               format: {
                                 with: DELIVERY_DESTINATION_FORMAT,
                                 message: "must be a Slack channel or user ID"
                               }
  validates :timezone, presence: true
  validate :cron_schedule_is_valid
  validate :owner_is_valid
  validate :delivery_destination_is_available

  def self.cron_for(preset, custom_expression = nil, custom_days: [], custom_time: nil)
    return if preset.to_s == "manual"
    return custom_expression.to_s.strip if preset.to_s == "cron"
    return custom_cron(custom_days, custom_time) if preset.to_s == "custom"

    SCHEDULE_PRESETS.fetch(preset.to_s, custom_expression.to_s.strip)
  end

  def self.custom_cron(days, time)
    ordered_days = CUSTOM_SCHEDULE_DAYS.filter_map do |(_label, value)|
      value if Array(days).map(&:to_s).include?(value)
    end
    match = /\A(?<hour>\d{2}):(?<minute>\d{2})\z/.match(time.to_s)
    return "" if ordered_days.empty? || match.blank?

    hour = match[:hour].to_i
    minute = match[:minute].to_i
    return "" unless hour.between?(0, 23) && minute.between?(0, 59)

    "#{minute} #{hour} * * #{ordered_days.join(',')}"
  end

  def schedule_preset
    return "manual" if cron_expression.blank?

    SCHEDULE_PRESETS.key(cron_expression) || "custom"
  end

  def schedule_label
    return "Manual only" if cron_expression.blank?

    preset_label = SCHEDULE_LABELS[schedule_preset]
    return preset_label if preset_label.present?

    custom_schedule_label || cron_expression
  end

  def custom_schedule_days
    simple_weekly_schedule&.fetch(:days, nil) || []
  end

  def custom_schedule_time
    simple_weekly_schedule&.fetch(:time, nil) || "09:00"
  end

  def next_occurrence(after: Time.current)
    parsed_cron&.next_time(after)&.to_t
  end

  def execution_principal
    principal || ConsoleUserPrincipalProvisioner.call(author)
  end

  def api_input
    unless delivery_destination_available?
      raise DeliveryDestinationUnavailable, "Slack delivery destination is no longer available to the task owner"
    end

    {
      prompt: prompt,
      principal: execution_principal.foreign_id,
      channel: delivery_channel,
      # Anyone in a channel can edit its tasks, so their output is not
      # attributed to a person.
      slack_user_id: author && SlackDeliveryPolicy.new(author).slack_user_id,
      scheduled_task_id: oid,
      scheduled_task_name: name
    }.compact
  end

  private

  def simple_weekly_schedule
    fields = cron_expression.to_s.split
    return unless fields.length == 5 && fields[2] == "*" && fields[3] == "*"
    return unless fields[0].match?(/\A\d+\z/) && fields[1].match?(/\A\d+\z/)

    minute = fields[0].to_i
    hour = fields[1].to_i
    return unless minute.between?(0, 59) && hour.between?(0, 23)

    days = expand_schedule_days(fields[4])
    return if days.empty?

    { days: days, time: format("%02d:%02d", hour, minute) }
  end

  def expand_schedule_days(day_expression)
    return CUSTOM_SCHEDULE_DAYS.map(&:last) if day_expression == "*"

    selected = day_expression.split(",").flat_map do |part|
      if (range = /\A([0-7])-([0-7])\z/.match(part))
        (range[1].to_i..range[2].to_i).map(&:to_s)
      elsif part.match?(/\A[0-7]\z/)
        [ part ]
      else
        []
      end
    end.map { |day| day == "7" ? "0" : day }.uniq

    CUSTOM_SCHEDULE_DAYS.filter_map { |(_label, value)| value if selected.include?(value) }
  end

  def custom_schedule_label
    schedule = simple_weekly_schedule
    return if schedule.blank?

    day_labels = CUSTOM_SCHEDULE_DAYS.filter_map do |label, value|
      label if schedule[:days].include?(value)
    end
    hour, minute = schedule[:time].split(":").map(&:to_i)
    meridiem = hour < 12 ? "AM" : "PM"
    display_hour = hour % 12
    display_hour = 12 if display_hour.zero?

    "#{day_labels.join(', ')} at #{display_hour}:#{format('%02d', minute)} #{meridiem} PT"
  end

  def parsed_cron
    Fugit::Cron.parse("#{cron_expression} #{timezone}")
  rescue ArgumentError
    nil
  end

  def set_channel_delivery
    self.delivery_channel = principal.slack_channel_id
  end

  def set_default_timezone
    self.timezone = DEFAULT_TIMEZONE if timezone.blank?
  end

  def schedule_requires_refresh?
    !enabled? || next_run_at.blank? || will_save_change_to_enabled? ||
      will_save_change_to_cron_expression? || will_save_change_to_timezone?
  end

  def refresh_next_run_at
    self.next_run_at = enabled? && cron_expression.present? ? next_occurrence : nil
  end

  def cron_schedule_is_valid
    return if cron_expression.blank?

    errors.add(:cron_expression, "is not a valid cron schedule") unless parsed_cron
  end

  def delivery_destination_available?
    if principal
      SlackBotChannel.active.exists?(channel_id: delivery_channel)
    else
      SlackDeliveryPolicy.new(author).allowed?(delivery_channel)
    end
  end

  def owner_is_valid
    unless [ author, principal ].compact.one?
      return errors.add(:base, "must be owned by exactly one of a user or a Slack channel")
    end
    return if principal.blank? || principal.kind == "slack_channel"

    errors.add(:principal, "must be a Slack channel")
  end

  def delivery_destination_is_available
    return if [ author, principal ].compact.empty? || delivery_channel.blank?
    return unless DELIVERY_DESTINATION_FORMAT.match?(delivery_channel)
    return if delivery_destination_available?

    errors.add(:delivery_channel, principal ? "is not a channel the Slack bot is in" : "is not available to the author")
  end
end
