module Api
  module V1
    module Console
      # A principal's role assignments for the console SPA (session only).
      class PrincipalRolesController < ::Api::V1::PrincipalRolesController
        include ConsoleApiAuthentication
      end
    end
  end
end
