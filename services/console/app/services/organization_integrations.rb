# Built-in connections administered once per console deployment. Registration
# controls discovery and OAuth ownership; credentials and grants still use the
# standard broker and role infrastructure. Never infer ownership from labels.
module OrganizationIntegrations
  Definition = Struct.new(:provider, :slug, :name, :icon, :description,
    :connection_class_name, :manage_route, :connect_route, keyword_init: true) do
    def connection = connection_class_name.constantize
  end

  def self.all
    [
      Definition.new(
        provider: "mercator", slug: Mercator::Connection::SLUG, name: "Mercator", icon: "🌐",
        description: "A shared wallet for your agents to discover and pay for external tools and services.",
        connection_class_name: "Mercator::Connection",
        manage_route: :console_mercator_path, connect_route: :connect_console_mercator_path
      )
    ]
  end

  def self.providers = all.map(&:provider)
  def self.for_provider(provider) = all.find { |integration| integration.provider == provider }
end
