# Presentation helpers for the secret-kind registry shared by the console's
# list, detail, and assignment views.
module ConsoleHelper
  def secret_kind_label(slug)
    SecretKinds::SECRET_KINDS.dig(slug, :label) || slug
  end

  def secret_kind_options
    SecretKinds::SECRET_KINDS.map { |slug, config| [ config[:label], slug ] }
  end

  def secret_form_kinds
    SecretKinds::SECRET_KINDS.select { |_slug, config| config[:form] }
  end
end
