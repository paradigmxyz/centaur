module Api
  module V1
    # Scheduled tasks.
    #
    # GET /api/v1/scheduled_tasks/:id (no ?view) is the admin-only execution
    # state lookup used by the workflow host: id, enabled, delivery_channel, and
    # never the prompt.
    #
    # Everything else backs the console Scheduled Tasks pages and is open to
    # any active user for the tasks they authored. Acting admins may also read
    # and manage other users' tasks (index ?scope=all). A task the caller may
    # not see is a 404.
    class ScheduledTasksController < Api::BaseController
      MAX_CHANNEL_OPTIONS = 20

      allow_active_users only: %i[index new show create update destroy run slack_channel_options]

      before_action :disable_caching
      before_action :require_admin_unless_console_view!, only: :show
      before_action :set_task, only: %i[show update destroy run], if: -> { action_name != "show" || console_view? }

      def index
        tasks = params[:scope] == "all" && acting_admin? ? authored_tasks : current_user.scheduled_tasks
        tasks = tasks.includes(:author, :principal).order(:name, :id).to_a
        names = slack_channel_names(tasks)
        render json: { data: tasks.map { |task| console_payload(task, names) } }
      end

      # Defaults for the new-task form: daily at 9:00 PT, enabled, and the
      # caller's Slack DM as the destination when one is linked.
      def new
        task = current_user.scheduled_tasks.new(cron_expression: ScheduledTask::SCHEDULE_PRESETS.fetch("daily"),
                                                 enabled: true)
        render json: { data: console_payload(task, {}) }
      end

      def show
        return render(json: { data: console_payload(@task) }) if console_view?

        task = ScheduledTask.find_by_oid!(params[:id])
        render json: { data: { id: task.oid, enabled: task.enabled?, delivery_channel: task.delivery_channel } }
      end

      def create
        task = current_user.scheduled_tasks.new
        save(task, status: :created)
      end

      def update
        save(@task, status: :ok)
      end

      def destroy
        @task.destroy!
        head :no_content
      end

      def run
        unless @task.enabled?
          return render_error(status: :unprocessable_entity, message: "Enable the task before running it.",
                              details: { reason: "task_disabled" })
        end

        unless ScheduledTaskRunJob.perform_later(@task.id, Time.current.iso8601)
          return render_error(status: :service_unavailable, message: "scheduled task run could not be queued")
        end

        render json: { data: console_payload(@task).merge(queued: true) }, status: :accepted
      end

      # Delivery channels for the task form: public channels plus private
      # channels both the author and the bot are in (SlackDeliveryPolicy). The
      # author is the caller, or the author of ?task_id= when editing a task.
      def slack_channel_options
        author = params[:task_id].present? ? task_author_for_options : current_user
        return if performed?

        policy = SlackDeliveryPolicy.new(author || current_user)

        SlackChannelCatalogSync.enqueue_if_empty
        channels = policy.allowed_channels.matching(params[:q]).ordered.limit(MAX_CHANNEL_OPTIONS)
        render json: {
          data: channels.map { |channel| channel_option(channel) },
          meta: { catalog_error: catalog_error(policy) }
        }
      end

      private

      def console_view?
        params[:view] == "console"
      end

      def disable_caching
        response.headers["Cache-Control"] = "no-store"
      end

      # The execution-state lookup keeps its admin-only contract.
      def require_admin_unless_console_view!
        require_admin! unless console_view?
      end

      # Slack channel tasks (no author) are managed from Slack, not here.
      def authored_tasks
        ScheduledTask.where.not(author_id: nil)
      end

      def set_task
        @task = authored_tasks.find_by_oid!(params[:id])
        require_owner_or_admin!(@task, owner: :author)
      end

      def task_author_for_options
        task = authored_tasks.find_by_oid!(params[:task_id])
        task.author if require_owner_or_admin!(task, owner: :author)
      end

      def save(task, status:)
        task.assign_attributes(ScheduledTaskConsoleAttributes.call(task, task_params))
        return render_validation_error(task) unless task.save

        render json: { data: console_payload(task) }, status: status
      end

      def task_params
        params.require(:data).permit(:name, :prompt, :enabled, :delivery_mode, :delivery_channel,
                                     :schedule_preset, :cron_expression, :custom_time, custom_days: [])
      end

      def console_payload(task, names = slack_channel_names([ task ]))
        dm_user_id = task.author && SlackDeliveryPolicy.new(task.author).direct_message_user_id
        delivers_to_dm = task.delivery_channel.present? && task.delivery_channel == dm_user_id
        {
          id: task.oid,
          name: task.name,
          prompt: task.prompt,
          author_id: task.author&.oid,
          author_email: task.author&.email,
          principal_id: task.principal&.oid,
          delivery_channel: task.delivery_channel.presence,
          delivery_mode: delivery_mode(task, dm_user_id),
          delivery_name: delivers_to_dm ? dm_name(task) : names[task.delivery_channel],
          direct_message_user_id: dm_user_id,
          cron_expression: task.cron_expression,
          timezone: task.timezone.presence || ScheduledTask::DEFAULT_TIMEZONE,
          schedule_preset: task.schedule_preset,
          schedule_label: task.schedule_label,
          custom_days: task.custom_schedule_days,
          custom_time: task.custom_schedule_time,
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

      # Legacy form rule: a user id destination is a DM, any other destination
      # a channel; a new task defaults to the DM when one is linked.
      def delivery_mode(task, dm_user_id)
        return "dm" if task.delivery_channel.to_s.start_with?("U", "W")
        return "channel" if task.delivery_channel.present?

        dm_user_id.present? ? "dm" : "channel"
      end

      def dm_name(task)
        task.author == current_user ? "Direct message to you" : "Direct message to #{task.author.email}"
      end

      def slack_channel_names(tasks)
        ids = tasks.map(&:delivery_channel).compact_blank.uniq
        SlackBotChannel.where(channel_id: ids).pluck(:channel_id, :name).to_h { |id, name| [ id, "##{name}" ] }
      rescue StandardError => e
        Rails.logger.warn("scheduled_task_slack_channels_load_failed error=#{e.class}: #{e.message}")
        {}
      end

      def channel_option(channel)
        {
          channel_id: channel.channel_id,
          name: channel.name,
          private: channel.private,
          label: "##{channel.name}",
          description: "#{channel.channel_id} · #{channel.private ? "Private" : "Public"}"
        }
      end

      def catalog_error(policy)
        return "SLACK_BOT_TOKEN is not configured." unless SlackChannelCatalogSync.configured?
        return "Slack channel catalog is loading. Enter a channel ID or reload shortly." if SlackBotChannel.none?
        return if policy.slack_team_id.blank?

        team_channels = SlackBotChannel.active.for_team(policy.slack_team_id).where(private: true)
        "Slack channel memberships are loading." if team_channels.where(membership_refreshed_at: nil).exists?
      end
    end
  end
end
