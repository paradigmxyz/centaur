module Api
  module V1
    module Sandbox
      class ScheduledTasksController < Api::SandboxBaseController
        include ScheduledTaskRunErrors

        class ChannelDeliveryMismatch < StandardError; end
        class PersonalDeliveryMismatch < StandardError; end

        PERSONAL_OWNER = "me".freeze

        rescue_from ChannelDeliveryMismatch do
          render_error(
            status: :unprocessable_entity,
            message: "tasks created in a Slack channel thread always deliver to that channel"
          )
        end

        rescue_from PersonalDeliveryMismatch do
          render_error(status: :unprocessable_entity, message: "personal tasks always deliver to your DM")
        end

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

          run = ScheduledTaskRunner.call(task)
          render json: { data: task_payload(task.reload).merge(queued: true, run_id: run["run_id"]) },
                 status: :accepted
        rescue CentaurApiClient::Error => e
          render_run_error(e)
        end

        private

        def disable_caching
          response.headers["Cache-Control"] = "no-store"
        end

        # A Slack channel session manages that channel's tasks on behalf of
        # whoever sent the current turn. A turn without a Slack requester, such
        # as a scheduled run, cannot manage them.
        #
        # With owner "me", a task is instead created for the requester's own
        # Console user and always delivers to their DM. From a channel this is
        # create-only: listing or editing personal tasks would expose them in
        # the shared thread, so they are managed from the DM.
        def require_task_owner!
          return unless valid_owner_param?

          if channel_principal && personal_request?
            return if personal_author

            return render_error(
              status: :forbidden,
              message: "personal tasks require a Slack requester linked to an active Console user"
            )
          end

          if channel_principal
            return if channel_requester

            return render_error(status: :forbidden, message: "channel tasks require a Slack user request")
          end
          return if task_author

          render_error(status: :forbidden, message: "sandbox principal is not linked to an active Console user")
        end

        def valid_owner_param?
          owner = requested_owner
          return true if owner.nil?

          unless owner == PERSONAL_OWNER
            render_error(status: :unprocessable_entity, message: %(owner must be "#{PERSONAL_OWNER}"))
            return false
          end
          return true if action_name == "create"

          render_error(status: :unprocessable_entity, message: "owner can only be set when creating a task")
          false
        end

        def requested_owner
          data = params[:data]
          data[:owner] if data.is_a?(ActionController::Parameters)
        end

        def personal_request?
          requested_owner == PERSONAL_OWNER
        end

        # The owner of a personal task comes only from the attested identity
        # of the current turn, never from a request parameter, so nobody can
        # create a task in another person's DM.
        def personal_author
          return @personal_author if defined?(@personal_author)
          return @personal_author = task_author unless channel_principal

          requester = channel_requester
          user = requester&.console_user
          @personal_author = user if user&.active? &&
            SlackDeliveryPolicy.new(user).direct_message_user_id == requester.slack_user_id
        end

        def channel_principal
          principal = current_proxy&.principal
          principal if principal&.kind == "slack_channel"
        end

        # Only a Slack user from a workspace the bot is installed in may act
        # for a channel, never a Slack Connect guest.
        def channel_requester
          requester = current_proxy.requester_principal
          return unless requester&.kind == "slack_dm" && requester.slack_user_id.present?

          requester if requester.slack_team_id.present? && SlackBotChannel.exists?(team_id: requester.slack_team_id)
        end

        def log_channel_task_request
          Rails.logger.info(
            "sandbox_channel_scheduled_task_request principal=#{channel_principal.oid} " \
            "requester=#{channel_requester&.oid} personal=#{personal_request?} " \
            "method=#{request.method} path=#{request.path} status=#{response.status}"
          )
        end

        def owned_tasks
          return personal_author.scheduled_tasks if personal_request?

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
          if personal_request?
            attributes[:delivery_channel] = personal_delivery(attributes[:delivery_channel])
          elsif channel_principal
            attributes[:delivery_channel] = channel_delivery(attributes[:delivery_channel])
          elsif attributes[:delivery_channel].to_s.strip.casecmp?("dm") ||
              (action_name == "create" && !attributes.key?(:delivery_channel))
            attributes[:delivery_channel] = SlackDeliveryPolicy.new(task_author).direct_message_user_id
          end
          attributes
        end

        # Channel tasks always deliver to their channel; refuse any other
        # destination rather than silently redirecting the output.
        def channel_delivery(requested)
          channel_id = channel_principal.slack_channel_id
          return channel_id if requested.blank? || requested.to_s.strip.casecmp?(channel_id)

          raise ChannelDeliveryMismatch
        end

        # Personal tasks always deliver to the author's DM; refuse any other
        # destination rather than silently redirecting the output.
        def personal_delivery(requested)
          dm_user_id = SlackDeliveryPolicy.new(personal_author).direct_message_user_id
          requested = requested.to_s.strip
          return dm_user_id if requested.blank? || requested.casecmp?("dm")
          return dm_user_id if dm_user_id.present? && requested.casecmp?(dm_user_id)

          raise PersonalDeliveryMismatch
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
