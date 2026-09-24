require "test_helper"
require "rbconfig"
require "tempfile"

module GoogleDocs
  class PdfTextExtractorTest < ActiveSupport::TestCase
    test "extracts and sanitizes text through a bounded subprocess" do
      with_file("%PDF fixture") do |pdf_path|
        with_extractor_script("File.binwrite(ARGV.last, \"Quarterly\\0results\\xFF\".b)") do |command|
          extracted = PdfTextExtractor.extract(pdf_path, command: command)

          assert_equal "Quarterlyresults", extracted
        end
      end
    end

    test "passes the page limit to the extractor" do
      with_file("%PDF fixture") do |pdf_path|
        with_extractor_script("File.write(ARGV.last, ARGV.join('|'))") do |command|
          extracted = PdfTextExtractor.extract(pdf_path, command: command, max_pages: 7)

          assert_includes extracted, "-l|7"
        end
      end
    end

    test "rejects extracted text over the byte limit" do
      with_file("%PDF fixture") do |pdf_path|
        with_extractor_script("File.write(ARGV.last, '123456')") do |command|
          assert_raises(PdfTextExtractor::Error) do
            PdfTextExtractor.extract(pdf_path, command: command, max_text_bytes: 5)
          end
        end
      end
    end

    test "rejects extractor failures" do
      with_file("%PDF fixture") do |pdf_path|
        with_extractor_script("exit 2") do |command|
          assert_raises(PdfTextExtractor::Error) do
            PdfTextExtractor.extract(pdf_path, command: command)
          end
        end
      end
    end

    test "terminates extraction after the wall-clock limit" do
      with_file("%PDF fixture") do |pdf_path|
        with_extractor_script("sleep 1") do |command|
          error = assert_raises(PdfTextExtractor::Error) do
            PdfTextExtractor.extract(pdf_path, command: command, timeout_seconds: 0.05)
          end

          assert_includes error.message, "exceeded"
        end
      end
    end

    private

    def with_file(contents)
      Tempfile.create("pdf-text-extractor-input-") do |file|
        file.binmode
        file.write(contents)
        file.flush
        yield file.path
      end
    end

    def with_extractor_script(body)
      Tempfile.create("pdf-text-extractor-command-") do |file|
        file.write("#!#{RbConfig.ruby}\n#{body}\n")
        file.flush
        File.chmod(0o700, file.path)
        path = file.path
        file.close
        yield path
      end
    end
  end
end
