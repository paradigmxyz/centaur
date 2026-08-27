module Api
  module V1
    module Sandbox
      class ScheduledTasksController < Api::SandboxBaseController
        before_action :require_linked_user!
        before_action :disable_caching

        def index
          tasks = task_author.scheduled_tasks.order(:name, :id)
          render json: { data: tasks.map { |task| task_payload(task) } }
        end

        def show
          render json: { data: task_payload(owned_task) }
        end

        def create
          task = task_author.scheduled_tasks.create!(task_attributes)
          render json: { data: task_payload(task) }, status: :created
        end

        def update
          task = owned_task
          task.update!(task_attributes)
          render json: { data: task_payload(task) }
        end

        def destroy
          owned_task.destroy!
          head :no_content
        end

        def run
          task = owned_task
          queued_job = ScheduledTaskRunJob.perform_later(task.id, Time.current.iso8601)
          unless queued_job
            return render_error(status: :service_unavailable, message: "scheduled task run could not be queued")
          end

          render json: { data: task_payload(task).merge(queued: true) }, status: :accepted
        end

        private

        def disable_caching
          response.headers["Cache-Control"] = "no-store"
        end

        def require_linked_user!
          return if task_author

          render_error(status: :forbidden, message: "sandbox principal is not linked to an active Console user")
        end

        def task_author
          return @task_author if defined?(@task_author)

          user = current_proxy.principal.console_user
          @task_author = user if user&.active?
        end

        def owned_task
          task_author.scheduled_tasks.find_by_oid!(params[:id])
        end

        def task_attributes
          attributes = params.require(:data).permit(
            :name,
            :prompt,
            :delivery_channel,
            :cron_expression,
            :enabled
          )
          if attributes[:delivery_channel].to_s.strip.casecmp?("dm")
            attributes[:delivery_channel] = SlackDeliveryPolicy.new(task_author).direct_message_user_id
          end
          attributes
        end

        def task_payload(task)
          {
            id: task.oid,
            name: task.name,
            prompt: task.prompt,
            delivery_channel: task.delivery_channel,
            cron_expression: task.cron_expression,
            timezone: task.timezone,
            schedule_label: task.schedule_label,
            enabled: task.enabled,
            next_run_at: task.next_run_at,
            last_enqueued_at: task.last_enqueued_at,
            last_run_id: task.last_run_id,
            last_run_at: task.last_run_at,
            last_error: task.last_error,
            created_at: task.created_at,
            updated_at: task.updated_at
          }
        end
      end
    end
  end
end
