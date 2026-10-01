class CrewProfile < ApplicationRecord
  belongs_to :principal, optional: true

  validates :crew_id, presence: true, uniqueness: true, format: { with: /\A[a-z][a-z0-9-]{1,47}\z/ }
  validates :app_id, uniqueness: true, format: { with: /\AA[A-Z0-9]+\z/ }, allow_nil: true
  validates :team_id, format: { with: /\AT[A-Z0-9]+\z/ }, allow_nil: true
  validate :configuration_shape

  def runtime_configuration
    { system_prompt: system_prompt, skills: skills, default_models: default_models }
  end

  # Called only by the admin Console. Slack owns app credentials; Console owns
  # the bot's configuration and its explicitly assigned credential roles.
  def provision!(record, user:, roles: nil)
    unless record["id"] == crew_id && record["app_id"].to_s.match?(/\AA[A-Z0-9]+\z/) &&
        record["team_id"].to_s.match?(/\AT[A-Z0-9]+\z/)
      raise SlackCrewClient::Error, "Slack app identity is not available yet. Reconcile installation before configuring this bot."
    end
    if persisted? && [ app_id, team_id ] != record.values_at("app_id", "team_id")
      raise SlackCrewClient::Error, "Crew app identity changed; refusing to move its access grants."
    end
    self.app_id = record["app_id"]
    self.team_id = record["team_id"]
    transaction do
      self.principal ||= Principal.create!(
        foreign_id: "slack-crew-#{team_id.downcase}-#{app_id.downcase}",
        name: "Crew · #{record['name']}", kind: "slack_crew", created_by: user,
        labels: { "managed-by" => "centaur-crew" }
      )
      save!
      if roles
        principal.principal_roles.where.not(role_id: roles.map(&:id)).find_each(&:destroy!)
        roles.each { |role| principal.principal_roles.find_or_create_by!(role: role) }
      end
    end
  end

  private

  def configuration_shape
    unless system_prompt.is_a?(String) && system_prompt.bytesize <= 64.kilobytes
      errors.add(:system_prompt, "must be text of at most 64 KiB")
    end
    unless default_models.is_a?(Hash) && (default_models.keys - %w[codex claude]).empty? &&
        default_models.values.all? { |value| value.is_a?(String) && value.bytesize <= 200 && !value.match?(/[\r\n\x00]/) }
      errors.add(:default_models, "must contain Codex/Claude model names of at most 200 bytes")
    end
    unless skills.is_a?(Array) && skills.size <= 32 && skills.all? { |skill| valid_skill?(skill) }
      errors.add(:skills, "must contain at most 32 skills with unique lowercase names, descriptions, and instructions (64 KiB each)")
      return
    end
    errors.add(:skills, "names must be unique") unless skills.map { |skill| skill["name"] }.uniq.size == skills.size
  end

  def valid_skill?(skill)
    skill.is_a?(Hash) && (skill.keys - %w[name description content]).empty? &&
      skill["name"].is_a?(String) && skill["name"].length <= 64 &&
      skill["name"].match?(/\A[a-z0-9]+(?:-[a-z0-9]+)*\z/) && skill["name"] != "search" &&
      skill["description"].is_a?(String) && skill["description"].present? && skill["description"].bytesize <= 1024 &&
      skill["content"].is_a?(String) && skill["content"].present? && skill["content"].bytesize <= 64.kilobytes
  end
end
