require "test_helper"

class Console::LocalFilesControllerTest < ActionDispatch::IntegrationTest
  test "redirects to login when not signed in" do
    get console_local_files_url
    assert_redirected_to login_path
  end

  test "a non-admin sees the local file workbench" do
    post login_url, params: { email: users(:member_user).email, password: "password123456" }

    get console_local_files_url
    assert_response :ok

    assert_select "[data-controller=local-files]"
    assert_select "[data-controller=pwa-install]"
  end
end
