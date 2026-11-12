module Api
  module V1
    # Console workflow dashboard (admin-only). Reads the api-rs workflow views
    # through WorkflowDashboard and force-starts runs through the workflows API.
    #
    #   GET  /api/v1/workflows               latest run per workflow (paginated)
    #   GET  /api/v1/workflows/:id           overview + debugging for one workflow
    #   GET  /api/v1/workflows/:id/runs      runs, ?status=&queue=&page=
    #   POST /api/v1/workflows/:id/runs      force-start a run
    #
    # :id is the workflow key (workflow name, or task name for older runs).
    # A missing or unreadable workflow database is a 503 with
    # details.reason=workflow_database_unavailable.
    class WorkflowsController < Api::BaseController
      accepts_credentials :session

      class_attribute :client_factory, default: -> { CentaurApiClient.new }

      rescue_from WorkflowDashboard::Unavailable do |e|
        render_error(status: :service_unavailable, message: e.message,
                     details: { reason: "workflow_database_unavailable" })
      end

      before_action :disable_caching

      def index
        rows, meta = dashboard.index(page: pagination_page)
        render json: { data: rows, meta: meta }
      end

      def show
        workflow = dashboard.show(workflow_name)
        return render_error(status: :not_found, message: "No workflow runs found for #{workflow_name}.") unless workflow

        render json: { data: workflow }
      end

      def runs
        rows, meta = dashboard.runs(workflow_name, status: params[:status].presence, queue: params[:queue].presence,
                                                   page: pagination_page)
        render json: { data: rows, meta: meta }
      end

      def create_run
        result = dashboard.force_start(workflow_name)
        render json: { data: result }, status: result[:created] ? :created : :ok
      rescue StandardError => e
        Rails.logger.warn("console_workflow_force_start_failed workflow=#{workflow_name} error=#{e.class}: #{e.message}")
        render_error(status: :bad_gateway, message: e.message, details: { reason: "workflow_api_error" })
      end

      private

      def workflow_name
        params[:id].to_s
      end

      def dashboard
        @dashboard ||= WorkflowDashboard.new(api_client: self.class.client_factory.call)
      end

      def disable_caching
        response.headers["Cache-Control"] = "no-store"
      end
    end
  end
end
