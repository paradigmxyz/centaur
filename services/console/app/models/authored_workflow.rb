require "fugit"

class AuthoredWorkflow < ApplicationRecord
  oid_prefix "awf"

  WORKFLOW_NAME = "console_workflow".freeze
  DEFAULT_TIMEZONE = "America/Los_Angeles".freeze
  SCHEDULE_PRESETS = {
    "hourly" => "0 * * * *",
    "daily" => "0 9 * * *",
    "weekdays" => "0 9 * * 1-5"
  }.freeze
  SCHEDULE_PRESET_LABELS = {
    "hourly" => "Hourly",
    "daily" => "Daily at 9:00 PT",
    "weekdays" => "Weekdays at 9:00 PT"
  }.freeze
  DELIVERY_CHANNEL_FORMAT = /\A[CDG][A-Z0-9]{8,}\z/

  belongs_to :author, class_name: "User"
  belongs_to :principal, optional: true

  scope :due, ->(now = Time.current) { where(enabled: true, next_run_at: ..now) }

  attr_writer :schedule_preset

  normalizes :name, :delivery_channel, :cron_expression, :timezone,
             with: ->(value) { value.to_s.strip }
  normalizes :prompt, with: ->(value) { value.to_s.gsub("\r\n", "\n").strip }

  before_validation :apply_schedule_preset
  before_validation :set_default_timezone
  before_validation :refresh_next_run_at, if: :schedule_requires_refresh?

  validates :name, presence: true, length: { maximum: 100 },
                   uniqueness: { scope: :author_id }
  validates :prompt, presence: true, length: { maximum: 64.kilobytes }
  validates :delivery_channel, presence: true,
                               format: {
                                 with: DELIVERY_CHANNEL_FORMAT,
                                 message: "must be a Slack channel ID"
                               }
  validates :cron_expression, presence: true
  validates :timezone, presence: true
  validate :cron_schedule_is_valid
  validate :selected_principal_has_foreign_id

  def self.cron_for(preset, custom_expression)
    return custom_expression.to_s.strip if preset.to_s == "cron"

    SCHEDULE_PRESETS.fetch(preset.to_s, custom_expression.to_s.strip)
  end

  def schedule_preset
    @schedule_preset.presence || SCHEDULE_PRESETS.key(cron_expression) || "cron"
  end

  def schedule_label
    SCHEDULE_PRESET_LABELS.fetch(schedule_preset, cron_expression)
  end

  def principal_oid
    principal&.oid
  end

  def next_occurrence(after: Time.current)
    parsed_cron&.next_time(after)&.to_t
  end

  def execution_principal
    principal || ConsoleUserPrincipalProvisioner.call(author)
  end

  def api_input
    {
      prompt: prompt,
      principal: execution_principal.foreign_id,
      channel: delivery_channel,
      authored_workflow_id: oid,
      authored_workflow_name: name
    }
  end

  private

  def apply_schedule_preset
    return if @schedule_preset.blank?

    preset = @schedule_preset
    @schedule_preset = nil
    self.cron_expression = self.class.cron_for(preset, cron_expression)
  end

  def parsed_cron
    Fugit::Cron.parse("#{cron_expression} #{timezone}")
  rescue ArgumentError
    nil
  end

  def set_default_timezone
    self.timezone = DEFAULT_TIMEZONE if timezone.blank?
  end

  def schedule_requires_refresh?
    !enabled? || next_run_at.blank? || will_save_change_to_enabled? ||
      will_save_change_to_cron_expression? || will_save_change_to_timezone?
  end

  def refresh_next_run_at
    self.next_run_at = enabled? ? next_occurrence : nil
  end

  def cron_schedule_is_valid
    errors.add(:cron_expression, "is not a valid cron schedule") unless parsed_cron
  end

  def selected_principal_has_foreign_id
    return if principal.blank? || principal.foreign_id.present?

    errors.add(:principal, "must have a foreign ID")
  end
end
