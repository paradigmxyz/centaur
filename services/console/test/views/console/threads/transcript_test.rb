require "test_helper"

class ConsoleThreadsTranscriptTest < ActionView::TestCase
  include ApplicationHelper

  test "renders attached images with intrinsic dimensions and lazy loading" do
    image_data = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
    render partial: "console/threads/transcript", locals: {
      items: [
        {
          source: :message,
          role: "user",
          label: "User",
          align: :end,
          text: "",
          images: [
            {
              src: "data:image/png;base64,#{image_data}",
              alt: "screenshot.png",
              width: 1440,
              height: 900
            }
          ],
          created_at: nil
        }
      ]
    }

    assert_select "img.console-message-image[src=?]", "data:image/png;base64,#{image_data}", count: 1
    assert_select "img.console-message-image[alt=?]", "screenshot.png", count: 1
    assert_select "img.console-message-image[width=?]", "1440", count: 1
    assert_select "img.console-message-image[height=?]", "900", count: 1
    assert_select "img.console-message-image[loading=?]", "lazy", count: 1
    assert_select "img.console-message-image[decoding=?]", "async", count: 1
    assert_select ".console-markdown", text: /No text content/, count: 0
  end
end
