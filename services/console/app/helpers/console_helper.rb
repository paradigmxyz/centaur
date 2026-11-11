# Presentation helpers for the secret-kind registry shared by the remaining
# legacy console views (role detail grants).
module ConsoleHelper
  def secret_kind_label(slug)
    SecretKinds::SECRET_KINDS.dig(slug, :label) || slug
  end
end
