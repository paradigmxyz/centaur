class Console::CrewController < ApplicationController
  layout "console"
  before_action :require_admin
  class_attribute :client_factory, default: -> { SlackCrewClient.new }

  rescue_from SlackCrewClient::Error do |error|
    redirect_to console_crew_index_path, alert: error.message
  end

  def index
    load_crew
    @configurations = CrewProfile.includes(principal: :roles).index_by(&:crew_id)
  rescue SlackCrewClient::Error => e
    @crew = []
    @configurations = {}
    flash.now[:alert] = e.message
    render :index, status: :bad_gateway
  end

  def new
    load_crew
    @bot = {}
    @configuration = CrewProfile.new(system_prompt: Rails.root.join("config/crew_system_prompt.md").read)
    prepare_editor
  rescue SlackCrewClient::Error => e
    redirect_to console_crew_index_path, alert: e.message
  end

  def edit
    load_bot
    prepare_editor
  rescue SlackCrewClient::Error => e
    redirect_to console_crew_index_path, alert: e.message
  end

  def create
    load_crew
    @bot = identity_params.to_h
    @configuration = CrewProfile.new(crew_id: @bot["id"], **configuration_params)
    prepare_editor(submitted_role_oids)
    @configuration.validate!
    roles = selected_roles
    @bot = client.create(@bot)
    @configuration.provision!(@bot, user: current_user, roles: roles)
    notice = @bot["status"] == "active" ? "Crew bot created and installed in Slack." : "Crew app exists; review its installation status."
    redirect_to edit_console_crew_path(@bot.fetch("id")), notice: notice
  rescue ActiveRecord::RecordInvalid, SlackCrewClient::Error, ActionController::ParameterMissing => e
    @configuration ||= CrewProfile.new
    @bot ||= {}
    prepare_editor(submitted_role_oids)
    flash.now[:alert] = e.message
    render :new, status: :unprocessable_entity
  end

  def update
    load_bot
    if params.require(:crew).keys == [ "paused" ]
      client.update(params[:id], "paused" => ActiveModel::Type::Boolean.new.cast(params[:crew][:paused]))
      return redirect_to edit_console_crew_path(params[:id]), notice: "Crew status updated."
    end
    prepare_editor(submitted_role_oids)
    CrewProfile.transaction do
      @configuration.lock! if @configuration.persisted?
      fields = configuration_params
      if @configuration.persisted? && fields["lock_version"].to_s != @configuration.lock_version.to_s
        raise ActiveRecord::StaleObjectError.new(@configuration, "update")
      end
      @configuration.revision_source = "console:#{current_user.oid}"
      @configuration.assign_attributes(fields)
      @configuration.validate!
      @configuration.provision!(@bot, user: current_user, roles: selected_roles)
      identity = identity_params.except(:id).to_h.reject { |key, value| @bot[key] == value }
      icon_url = params.require(:crew).permit(:icon_url)[:icon_url].to_s.strip
      identity["icon_url"] = icon_url if icon_url.present? && icon_url != @bot["icon_url"]
      @bot = client.update(params[:id], identity) if identity.any?
    end
    redirect_to edit_console_crew_path(params[:id]), notice: "Crew saved. Behavior changes apply when a sandbox is next created or rebuilt."
  rescue ActiveRecord::StaleObjectError
    redirect_to edit_console_crew_path(params[:id]), alert: "This bot changed since you opened it. Review the latest settings before saving."
  rescue ActiveRecord::RecordInvalid, SlackCrewClient::Error, ActionController::ParameterMissing => e
    return redirect_to console_crew_index_path, alert: e.message unless @configuration

    prepare_editor(submitted_role_oids)
    flash.now[:alert] = e.message
    render :edit, status: :unprocessable_entity
  end

  def install
    bot = client.install(params[:id])
    configuration = CrewProfile.find_or_initialize_by(crew_id: params[:id])
    configuration.provision!(bot, user: current_user)
    redirect_to edit_console_crew_path(params[:id]), notice: "Crew bot installed in Slack."
  rescue SlackCrewClient::Error, ActiveRecord::RecordInvalid => e
    redirect_to edit_console_crew_path(params[:id]), alert: e.message
  end

  def memories
    load_bot
    @memories = @configuration.memories.order(updated_at: :desc, id: :desc)
  end

  def remember
    load_bot
    raise ActiveRecord::RecordNotFound, "Install and configure this bot first" unless @configuration.persisted?
    fields = params.require(:memory).permit(:scope_key, :key, :content, :source, :expires_at, :lock_version).to_h
    @configuration.remember!(scope_key: fields["scope_key"], key: fields["key"], attributes: fields,
      expected_version: fields["lock_version"])
    redirect_to memories_console_crew_path(params[:id]), notice: "Memory saved. It is available immediately to this bot in its selected scope."
  rescue ActiveRecord::RecordInvalid, ActiveRecord::StaleObjectError => e
    redirect_to memories_console_crew_path(params[:id]), alert: e.is_a?(ActiveRecord::StaleObjectError) ? "Memory changed; review the latest version before saving." : e.message
  end

  def forget
    load_bot
    fields = params.require(:memory).permit(:scope_key, :key, :lock_version)
    @configuration.forget!(scope_key: fields[:scope_key], key: fields[:key], expected_version: fields[:lock_version])
    redirect_to memories_console_crew_path(params[:id]), notice: "Memory forgotten. Existing conversations may still contain previously read copies."
  rescue ActiveRecord::StaleObjectError
    redirect_to memories_console_crew_path(params[:id]), alert: "Memory changed; review the latest version before deleting."
  end

  def history
    load_bot
    @revisions = @configuration.revisions.order(version: :desc)
    @revision = @revisions.find_by!(version: params[:version]) if params[:version].present?
  end

  def restore
    load_bot
    @configuration.revision_source = "console:#{current_user.oid}"
    @configuration.restore!(params.require(:version), expected_version: params[:lock_version])
    redirect_to history_console_crew_path(params[:id]), notice: "Behavior restored. Applies to new or rebuilt sandboxes; access grants and memories are unchanged."
  rescue ActiveRecord::StaleObjectError
    redirect_to history_console_crew_path(params[:id]), alert: "Behavior changed; review the latest version before restoring."
  end

  private

  def client
    @client ||= self.class.client_factory.call
  end

  def load_crew
    result = client.list
    @crew = Array(result["crew"])
  end

  def load_bot
    load_crew
    @bot = @crew.find { |record| record["id"] == params[:id] }
    raise ActiveRecord::RecordNotFound, "Crew bot not found" unless @bot

    @configuration = CrewProfile.find_or_initialize_by(crew_id: params[:id])
  end

  def identity_params
    params.require(:crew).permit(:id, :name, :description)
  end

  def configuration_params
    fields = params.require(:crew).permit(:system_prompt, :lock_version, default_models: %i[codex claude], skills: %i[name description content]).to_h
    # Nested-fields submits an indexed hash. Removing every row is an explicit
    # replacement with an empty list, not a request to retain previous skills.
    fields["skills"] = fields.fetch("skills", {}).values
    fields
  end

  def submitted_role_oids
    Array(params.dig(:crew, :role_oids)).reject(&:blank?)
  end

  def prepare_editor(role_oids = nil)
    @roles = Role.order(:name, :id)
    @selected_role_oids = role_oids || @configuration.principal&.roles&.map(&:oid) || []
  end

  def selected_roles
    @selected_role_oids.uniq.map { |oid| Role.find_by_oid!(oid) }
  end
end
