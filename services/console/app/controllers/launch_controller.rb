# Entry point for the web+centaur:// protocol handler the PWA manifest
# registers. When the OS opens such a link, the installed app navigates here
# with the full custom-scheme URL in ?target=; we map it onto an in-app path
# and redirect. web+centaur://console/threads lands on /console/threads.
#
# Only strictly path-shaped targets survive the mapping (no dots, queries, or
# protocol-relative tricks), so a crafted link can never bounce the operator
# off-origin. Anything that doesn't parse falls back to the console root.
class LaunchController < ApplicationController
  SCHEME_PREFIX = "web+centaur://".freeze
  SAFE_PATH = %r{\A[A-Za-z0-9_/-]+\z}

  def show
    redirect_to launch_path_for(params[:target].to_s)
  end

  private

  def launch_path_for(target)
    rest = target.delete_prefix(SCHEME_PREFIX)
    return root_path if rest == target || rest.blank?

    # Collapse and trim slashes before re-rooting the path: "/#{path}" must
    # never come out protocol-relative ("//host") or dot-traversable.
    path = rest.squeeze("/").delete_prefix("/").delete_suffix("/")
    return root_path unless path.match?(SAFE_PATH)

    "/#{path}"
  end
end
