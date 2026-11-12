module Api
  module V1
    module Console
      # Grant create/show/destroy for the console SPA (session only). Payloads
      # add a `grantable` summary (kind, kind_label, id, foreign_id, name).
      class GrantsController < ::Api::V1::GrantsController
        include ConsoleApiAuthentication

        private

        def serialize(grant)
          payload = super
          payload[:data][:grantable] = GrantableSummary.call(grant.grantable)
          payload
        end
      end
    end
  end
end
