require "tempfile"

module GoogleDocs
  class PdfTextExtractor
    class Error < StandardError; end

    COMMAND = "/usr/bin/pdftotext"
    MAX_PAGES = 1_000
    # Keep text_content safely below PostgreSQL's 1 MiB to_tsvector input limit.
    MAX_TEXT_BYTES = 750 * 1024
    TIMEOUT_SECONDS = 60
    MEMORY_LIMIT_BYTES = 512 * 1024 * 1024
    POLL_INTERVAL_SECONDS = 0.05

    def self.extract(
      path,
      command: COMMAND,
      max_pages: MAX_PAGES,
      max_text_bytes: MAX_TEXT_BYTES,
      timeout_seconds: TIMEOUT_SECONDS,
      memory_limit_bytes: MEMORY_LIMIT_BYTES
    )
      Tempfile.create([ "google-drive-pdf-text-", ".txt" ]) do |output|
        output.close
        options = {
          unsetenv_others: true,
          pgroup: true,
          out: File::NULL,
          err: File::NULL,
          rlimit_cpu: [ timeout_seconds.ceil, timeout_seconds.ceil + 1 ]
        }
        # macOS rejects RLIMIT_AS in posix_spawn; production runs on Linux,
        # where this contains decompression bombs to the extractor process.
        options[:rlimit_as] = memory_limit_bytes unless RUBY_PLATFORM.include?("darwin")
        pid = Process.spawn(
          {},
          command,
          "-f", "1",
          "-l", max_pages.to_s,
          "-enc", "UTF-8",
          "-nopgbrk",
          path,
          output.path,
          **options
        )
        status = wait_for_process(
          pid,
          output.path,
          max_text_bytes: max_text_bytes,
          timeout_seconds: timeout_seconds
        )
        raise Error, "PDF text extraction failed" unless status.success?
        raise Error, "PDF text exceeds the indexing limit" if File.size(output.path) > max_text_bytes

        sanitize(File.binread(output.path))
      end
    rescue Error
      raise
    rescue SystemCallError => error
      raise Error, "could not run PDF text extraction: #{error.class}"
    end

    def self.wait_for_process(pid, output_path, max_text_bytes:, timeout_seconds:)
      deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + timeout_seconds
      loop do
        waited_pid, status = Process.waitpid2(pid, Process::WNOHANG)
        return status if waited_pid

        if File.size?(output_path).to_i > max_text_bytes
          terminate_process_group(pid)
          raise Error, "PDF text exceeds the indexing limit"
        end
        if Process.clock_gettime(Process::CLOCK_MONOTONIC) >= deadline
          terminate_process_group(pid)
          raise Error, "PDF text extraction exceeded #{timeout_seconds} seconds"
        end
        sleep(POLL_INTERVAL_SECONDS)
      end
    rescue Errno::ECHILD
      raise Error, "PDF text extraction process exited unexpectedly"
    end
    private_class_method :wait_for_process

    def self.terminate_process_group(pid)
      signal_process("TERM", pid)
      sleep(POLL_INTERVAL_SECONDS)
      signal_process("KILL", pid)
    ensure
      begin
        Process.wait(pid)
      rescue Errno::ECHILD
        nil
      end
    end
    private_class_method :terminate_process_group

    def self.signal_process(signal, pid)
      Process.kill(signal, -pid)
    rescue Errno::EPERM
      begin
        Process.kill(signal, pid)
      rescue Errno::ESRCH
        nil
      end
    rescue Errno::ESRCH
      nil
    end
    private_class_method :signal_process

    def self.sanitize(text)
      text.to_s.encode(Encoding::UTF_8, invalid: :replace, undef: :replace, replace: "").delete("\u0000")
    end
    private_class_method :sanitize
  end
end
