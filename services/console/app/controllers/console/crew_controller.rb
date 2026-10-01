class Console::CrewController < ApplicationController
  layout "console"

  before_action :require_admin

  class_attribute :client_factory, default: -> { SlackCrewClient.new }

  def index
    load_crew
    @form = {}
  rescue SlackCrewClient::Error => e
    service_unavailable(e)
  end

  def create
    @form = create_params.to_h
    record = client.create(@form)
    if record["status"] == "active"
      return redirect_to console_crew_index_path, notice: "Crew bot is already installed."
    end
    redirect_to validated_install_url!(record.fetch("install_url"), @form.fetch("id")), allow_other_host: true
  rescue ActionController::ParameterMissing, KeyError => e
    recover_create(e.message)
  rescue SlackCrewClient::Error => e
    recover_create(e.message)
  rescue InvalidInstallUrl
    recover_create("Crew was created, but the Slack install URL was invalid. Use Install in Slack to retry.")
  end

  def update
    attributes = update_params.to_h
    attributes["paused"] = ActiveModel::Type::Boolean.new.cast(attributes["paused"]) if attributes.key?("paused")
    client.update(params[:id], attributes)
    redirect_to console_crew_index_path, notice: "Crew bot updated."
  rescue SlackCrewClient::Error, ActionController::ParameterMissing => e
    redirect_to console_crew_index_path, alert: e.message
  end

  def install
    record = Array(client.list["crew"]).find { |item| item["id"].to_s == params[:id].to_s }
    raise SlackCrewClient::Error, "Crew bot was not found" unless record

    redirect_to validated_install_url!(record["install_url"], params[:id]), allow_other_host: true
  rescue SlackCrewClient::Error, InvalidInstallUrl => e
    redirect_to console_crew_index_path, alert: e.message
  end

  private

  InvalidInstallUrl = Class.new(StandardError)

  def client
    @client ||= self.class.client_factory.call
  end

  def create_params
    params.require(:crew).permit(:id, :name, :crew_id, :description)
  end

  def update_params
    params.require(:crew).permit(:name, :description, :paused)
  end

  def load_crew
    result = client.list
    @crew = Array(result["crew"])
    @profiles = Array(result["profiles"])
  end

  def recover_create(message)
    @form ||= {}
    load_crew
    flash.now[:alert] = message
    render :index, status: :unprocessable_entity
  rescue SlackCrewClient::Error => e
    service_unavailable(e, status: :unprocessable_entity)
  end

  def service_unavailable(error, status: :bad_gateway)
    @crew = []
    @profiles = []
    @form ||= {}
    flash.now[:alert] = error.message
    render :index, status: status
  end

  def validated_install_url!(raw_url, id)
    public_uri = URI.parse(ConsoleEnv["SLACK_CREW_PUBLIC_URL"].to_s)
    install_uri = URI.parse(raw_url.to_s)
    expected_path = "/api/slack/crew/#{CGI.escape(id.to_s)}/install"
    valid_origin = install_uri.scheme == "https" && public_uri.scheme == "https" &&
      install_uri.host == public_uri.host && install_uri.port == public_uri.port
    query = URI.decode_www_form(install_uri.query.to_s)
    valid_query = query.any? { |key, value| key == "ticket" && value.present? } && query.none? { |key, _| key == "return_url" }
    raise InvalidInstallUrl, "Slack install URL was invalid" unless valid_origin && install_uri.userinfo.nil? && install_uri.path == expected_path && valid_query && install_uri.fragment.nil?

    install_uri.to_s
  rescue URI::InvalidURIError
    raise InvalidInstallUrl, "Slack install URL was invalid"
  end
end
