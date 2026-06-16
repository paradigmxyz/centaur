module Console
  # View helpers for the operator (console user) management screen.
  module UsersHelper
    STATUS_CHIP_CLASSES = {
      "active" => "bg-emerald-500/10 text-emerald-300 ring-emerald-500/20",
      "pending" => "bg-amber-500/10 text-amber-300 ring-amber-500/20",
      "disabled" => "bg-zinc-500/10 text-zinc-400 ring-zinc-500/20"
    }.freeze

    IDP_CHIP_CLASSES = "bg-ink-700/60 text-zinc-300 ring-ink-500".freeze

    # A colored pill for a user's account status.
    def user_status_badge(user)
      chip(user.status, STATUS_CHIP_CLASSES.fetch(user.status, STATUS_CHIP_CLASSES["disabled"]))
    end

    # One pill per linked identity provider, or a single "password" pill when the
    # account has no SSO identity. All share the same neutral IdP styling.
    def user_idp_chips(user)
      labels = user.user_identities.map(&:provider).uniq.sort
      labels = %w[password] if labels.empty?
      tag.div(safe_join(labels.map { |label| chip(label.capitalize, IDP_CHIP_CLASSES) }), class: "flex flex-wrap gap-1")
    end

    private

    def chip(text, classes)
      tag.span(text, class: "rounded px-1.5 py-0.5 text-xs ring-1 #{classes}")
    end
  end
end
