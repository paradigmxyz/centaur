# Be sure to restart your server when you modify this file.

# Enforcing Content-Security-Policy for every response Rails serves (the
# middleware sets it on HTML, JSON, and redirects alike; files under public/
# are served before it and carry none). This is the policy of the console SPA
# shell (SpaController): its scripts, styles, fonts, and API calls are all
# same-origin, so a stray XSS can neither load or inline script nor send data
# to another origin. See services/console/AGENTS.md ("Content-Security-Policy").
#
# - script-src 'self' with no inline or eval allowance: web/index.html has no
#   inline script (the pre-paint theme script is an emitted file, see
#   web/vite.config.ts) and zod runs jitless (web/src/main.tsx).
# - style-src-elem allows inline <style> elements: sonner and Radix's scroll
#   lock (react-remove-scroll-bar) inject them at runtime, one with computed
#   content. style="" attributes stay blocked (style-src-attr falls back to
#   style-src 'self'); React sets inline styles through the CSSOM, which CSP
#   does not govern. Injected CSS cannot exfiltrate anything: every fetch
#   directive (img, font, connect, default) is 'self'.
# - No form-action: the MCP consent form's POST redirects to arbitrary
#   registered client redirect URIs (loopback http, custom schemes), and
#   browsers enforce form-action on that redirect. Do not add it globally.
#
# The server-rendered sign-in and OAuth pages tighten this further
# (ServerRenderedPagePolicy: no script at all, no inline style).
Rails.application.configure do
  config.content_security_policy do |policy|
    policy.default_src :self
    policy.script_src :self
    policy.style_src :self
    policy.style_src_elem :self, :unsafe_inline
    policy.img_src :self, :data
    policy.font_src :self
    policy.connect_src :self
    policy.worker_src :self
    policy.manifest_src :self
    policy.object_src :none
    policy.base_uri :none
    policy.frame_ancestors :none
  end
end
