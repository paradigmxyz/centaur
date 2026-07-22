class Console::EtlsController < ApplicationController
  layout "console"

  before_action :require_admin

  class_attribute :client_factory, default: -> { CentaurApiClient.new }

  def index
    @archive_imports = api_client.list_slack_archive_imports(limit: 100).fetch("imports", [])
  rescue CentaurApiClient::Error => e
    @archive_imports = []
    flash.now[:alert] = e.message
  end

  def create_slack_archive_import
    result = api_client.create_slack_archive_import(
      filename: params.require(:filename),
      content_type: params[:content_type].presence || "application/zip",
      created_by: current_user&.email || "console",
      metadata: { source: "centaur_console" }
    )
    render json: result, status: :created
  rescue ActionController::ParameterMissing => e
    render json: { ok: false, error: e.message }, status: :unprocessable_entity
  rescue CentaurApiClient::Error => e
    render json: { ok: false, error: e.message }, status: :bad_gateway
  end

  def start_slack_archive_import
    api_client.start_slack_archive_import(params.require(:import_id))
    redirect_to console_etls_path, notice: "Archive ingest started."
  rescue CentaurApiClient::Error => e
    redirect_to console_etls_path, alert: e.message
  end

  def retry_slack_archive_import
    api_client.retry_slack_archive_import(params.require(:import_id))
    redirect_to console_etls_path, notice: "Archive ingest retry started."
  rescue CentaurApiClient::Error => e
    redirect_to console_etls_path, alert: e.message
  end

  def delete_slack_archive_import
    api_client.delete_slack_archive_import(params.require(:import_id))
    redirect_to console_etls_path, notice: "Archive import deleted."
  rescue CentaurApiClient::Error => e
    redirect_to console_etls_path, alert: e.message
  end

  private

  def api_client
    @api_client ||= self.class.client_factory.call
  end
end
