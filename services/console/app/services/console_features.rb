module ConsoleFeatures
  module_function

  def chat_enabled?
    raw = ConsoleEnv["CHAT_ENABLED"]
    raw.nil? || ActiveModel::Type::Boolean.new.cast(raw)
  end
end
