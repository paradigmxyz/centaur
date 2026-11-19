# Renders api-rs's refusal to start a scheduled task run. A 422 explains why the
# task cannot run, such as a delivery destination its owner lost; anything
# else means the run could not be started right now.
module ScheduledTaskRunErrors
  private

  def render_run_error(error)
    if error.status == 422
      render_error(status: :unprocessable_entity, message: error.message)
    else
      render_error(status: :service_unavailable, message: "scheduled task run could not be started")
    end
  end
end
