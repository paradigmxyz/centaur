class Console::CrewController < ApplicationController
  layout "console"
  before_action :require_admin
  class_attribute :client_factory, default: -> { SlackCrewClient.new }

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
    @configuration = CrewProfile.new
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
    @profiles ||= []
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
      @configuration.assign_attributes(fields)
      @configuration.validate!
      @configuration.provision!(@bot, user: current_user, roles: selected_roles)
      identity = identity_params.except(:id, :crew_id).to_h.reject { |key, value| @bot[key] == value }
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

  private

  def client
    @client ||= self.class.client_factory.call
  end

  def load_crew
    result = client.list
    @crew = Array(result["crew"])
    @profiles = Array(result["profiles"])
  end

  def load_bot
    load_crew
    @bot = @crew.find { |record| record["id"] == params[:id] }
    raise ActiveRecord::RecordNotFound, "Crew bot not found" unless @bot

    @configuration = CrewProfile.find_or_initialize_by(crew_id: params[:id])
  end

  def identity_params
    params.require(:crew).permit(:id, :name, :crew_id, :description)
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
