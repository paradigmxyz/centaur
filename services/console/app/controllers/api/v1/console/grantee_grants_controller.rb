module Api
  module V1
    module Console
      # A principal's or role's grants for the console SPA (session only).
      # Payloads add the `grantable` summary GrantsController returns.
      class GranteeGrantsController < ::Api::V1::GranteeGrantsController
        include ConsoleApiAuthentication

        private

        def grant_payload(grant)
          super.merge(grantable: GrantableSummary.call(grant.grantable))
        end
      end
    end
  end
end
