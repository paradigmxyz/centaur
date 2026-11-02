module Api
  module V1
    module Sandbox
      module Admin
        # Direct principal grants and role grants. A role grant changes every
        # holder of the role.
        class GrantsController < ::Api::V1::GrantsController
          include SandboxAdminAuthentication
        end
      end
    end
  end
end
