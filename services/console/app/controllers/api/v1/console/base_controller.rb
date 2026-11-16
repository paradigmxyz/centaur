module Api
  module V1
    module Console
      # Base for console API endpoints that have no bearer counterpart. Session
      # only; see ConsoleApiAuthentication. Console views of a bearer resource
      # subclass the bearer controller and include ConsoleApiAuthentication
      # instead.
      class BaseController < ::Api::BaseController
        include ConsoleApiAuthentication

        private

        # The Principal or Role named by a "principal:<oid>" / "role:<oid>" query
        # param, or nil when the param is absent. Unknown oids are 404s.
        def principal_or_role_param!(name)
          raw = params[name].presence
          return unless raw

          type, oid = raw.to_s.split(":", 2)
          case type
          when "principal" then Principal.find_by_oid!(oid)
          when "role" then Role.find_by_oid!(oid)
          else raise ActionController::BadRequest, "#{name} must be principal:<oid> or role:<oid>"
          end
        end

        # One Slack channel as the console's channel pickers show it.
        def slack_channel_option(channel)
          {
            channel_id: channel.channel_id,
            name: channel.name,
            private: channel.private,
            label: "##{channel.name}",
            description: "#{channel.channel_id} · #{channel.private ? "Private" : "Public"}"
          }
        end
      end
    end
  end
end
