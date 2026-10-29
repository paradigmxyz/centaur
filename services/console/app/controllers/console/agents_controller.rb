class Console::AgentsController < ApplicationController
  layout "console"
  before_action :require_admin

  class_attribute :client_factory, default: -> { CentaurApiClient.new }
  class_attribute :channel_defaults_factory, default: -> { SlackChannelDefaults.current }

  # The personas api-rs loaded at startup, with the Slack channels that default
  # to each. Channels naming a persona api-rs did not load are listed apart so
  # a typo or an unmerged persona is visible.
  def index
    @api_unavailable = false
    @personas = []
    @default_persona_id = nil
    @channel_defaults = channel_defaults
    @channel_principals = channel_principals(@channel_defaults.entries.keys)

    response = api_client.list_personas
    @personas = Array(response["personas"]).select { |persona| persona.is_a?(Hash) }
    @default_persona_id = response["default_persona_id"].presence
    deployed = @personas.map { |persona| persona["id"] }
    @undeployed_channels = @channel_defaults.entries.reject do |_channel_id, fields|
      fields["persona"].blank? || deployed.include?(fields["persona"])
    end
  rescue StandardError => e
    Rails.logger.warn("console_agents_load_failed error=#{e.class}: #{e.message}")
    @api_unavailable = true
    @undeployed_channels = {}
  end

  def show
    @api_unavailable = false
    @persona_id = params[:id].to_s
    @channel_defaults = channel_defaults
    @channels = @channel_defaults.channels_for_persona(@persona_id)
    @channel_principals = channel_principals(@channels.keys)

    persona = api_client.get_persona(@persona_id)["persona"]
    @persona = persona.is_a?(Hash) ? persona : nil
    response.status = :not_found if @persona.nil?
  rescue CentaurApiClient::Error => e
    if e.status == 404
      response.status = :not_found
    else
      Rails.logger.warn("console_agent_load_failed persona=#{@persona_id} error=#{e.class}: #{e.message}")
      @api_unavailable = true
    end
  rescue StandardError => e
    Rails.logger.warn("console_agent_load_failed persona=#{@persona_id} error=#{e.class}: #{e.message}")
    @api_unavailable = true
  end

  private

  def channel_defaults
    self.class.channel_defaults_factory.call
  end

  def channel_principals(channel_ids)
    return {} if channel_ids.empty?

    Principal.where(slack_channel_id: channel_ids).index_by(&:slack_channel_id)
  end

  def api_client
    @api_client ||= self.class.client_factory.call
  end
end
