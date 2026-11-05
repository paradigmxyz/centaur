# Switch for API clients: machine callers of the api-rs session API. Off, the
# Console hides and refuses their management pages; api-rs separately refuses
# their tokens. Rows already created are kept.
module ApiClientsFeature
  module_function

  def enabled?
    ActiveModel::Type::Boolean.new.cast(ConsoleEnv["API_CLIENTS_ENABLED"]) == true
  end
end
