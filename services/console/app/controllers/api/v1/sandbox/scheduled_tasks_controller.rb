module Api
  module V1
    module Sandbox
      class ScheduledTasksController < Api::SandboxBaseController
        before_action :require_task_owner!
        before_action :disable_caching
        after_action :log_channel_task_request, if: :channel_principal

        def index
          tasks = owned_tasks.order(:name, :id)
          render json: { data: tasks.map { |task| task_payload(task) } }
        end

        def show
          render json: { data: task_payload(owned_task) }
        end

        def create
          task = owned_tasks.create!(task_attributes)
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
          unless task.enabled?
            return render_error(status: :unprocessable_entity, message: "scheduled task is disabled")
          end

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

        # A Slack channel session manages that channel's tasks on behalf of
        # whoever sent the current turn. A turn without a Slack requester, such
        # as a scheduled run, cannot manage them.
        def require_task_owner!
          if channel_principal
            return if channel_requester

            return render_error(status: :forbidden, message: "channel tasks require a Slack user request")
          end
          return if task_author

          render_error(status: :forbidden, message: "sandbox principal is not linked to an active Console user")
        end

        def channel_principal
          principal = current_proxy&.principal
          principal if principal&.kind == "slack_channel"
        end

        def channel_requester
          requester = current_proxy.requester_principal
          requester if requester&.kind == "slack_dm" && requester.slack_user_id.present?
        end

        def log_channel_task_request
          Rails.logger.info(
            "sandbox_channel_scheduled_task_request principal=#{channel_principal.oid} " \
            "requester=#{channel_requester&.oid} method=#{request.method} path=#{request.path} " \
            "status=#{response.status}"
          )
        end

        def owned_tasks
          channel_principal ? channel_principal.scheduled_tasks : task_author.scheduled_tasks
        end

        def task_author
          return @task_author if defined?(@task_author)

          user = current_proxy.principal.console_user
          @task_author = user if user&.active?
        end

        def owned_task
          owned_tasks.find_by_oid!(params[:id])
        end

        def task_attributes
          attributes = params.require(:data).permit(
            :name,
            :prompt,
            :delivery_channel,
            :cron_expression,
            :enabled
          )
          # Channel tasks always deliver to their channel.
          if channel_principal
            attributes.delete(:delivery_channel)
          elsif attributes[:delivery_channel].to_s.strip.casecmp?("dm")
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
