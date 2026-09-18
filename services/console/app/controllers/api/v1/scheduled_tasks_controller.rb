module Api
  module V1
    class ScheduledTasksController < Api::BaseController
      def show
        response.headers["Cache-Control"] = "no-store"
        task = ScheduledTask.find_by_oid!(params[:id])
        render json: {
          data: {
            id: task.oid,
            enabled: task.enabled?,
            delivery_channel: task.delivery_channel
          }
        }
      end
    end
  end
end
