# Local file workbench: browse, preview, and edit files on the operator's own
# machine via the File System Access API. Everything happens client-side in the
# Stimulus controller -- no file content ever reaches the server. The page is
# also the manifest file_handlers target, so files opened with the installed
# PWA ("Open with Centaur Console") arrive here via window.launchQueue.
#
# Not admin-gated (like Integrations): the page only touches the signed-in
# user's own disk, never server-side state.
class Console::LocalFilesController < ApplicationController
  layout "console"

  def index
  end
end
