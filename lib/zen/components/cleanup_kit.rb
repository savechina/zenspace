# typed: false

module Zen
  module Components
    class CleanupKit
      include Import["logger", "settings"]

      # Cleans up various system and application files to free up space and maintain system hygiene.
      #
      # This method performs the following cleanup tasks:
      # - Deletes logs and cache files from the system Trash directories.
      # - Removes diagnostic reports from the system logs.
      # - Clears the Go module cache using the `go clean -modcache` command.
      # - Cleans up RubyGems cache using the `gem cleanup` command.
      # - Cleans up Homebrew cache using the `brew cleanup -s` command.
      # - Deletes Java heap dump files (*.hprof) from the user's home directory.
      # - Clears all application log files from JetBrains IDEs.
      #
      # Note:
      # - Ensure you have the necessary permissions to delete files in the specified directories.
      # - Use this method with caution as it performs irreversible deletions.
      def clean_all
        # logger.info("entry repo clean ")
        # Add your cleanup logic here, such as deleting logs or cache files.

        puts "Cleaning logs and caches..."

        puts "Cleaning system Trashes"
        puts File.join("~/.Trash/*")
        puts File.join("/Volumes/*/.Trashes/*")

        # cleanup Trash
        system("osascript", "-e", 'tell application "Finder" to empty trash')

        FileUtils.rm_rf(File.join("~/.Trash/*"))
        FileUtils.rm_rf(File.join("/Volumes/*/.Trashes/*"))
        FileUtils.rm_rf(File.join("/Library/Logs/DiagnosticReports/*"))
        # FileUtils.rm_rf(File.join("~/Library/Logs/*"))
        # FileUtils.rm_rf(File.join(@root, @workspace, 'cache'))
        puts "Clearing Go module cache"
        system("go clean -modcache")

        puts "Cleaning RubyGems cache"
        system("gem cleanup")

        puts "Cleaning Homebrew cache"
        system("brew cleanup -s")

        puts "Deleting Java heap dumps"
        FileUtils.rm_rf(File.join("~/*.hprof"))

        puts "Clearing all application log files from JetBrains"
        FileUtils.rm_rf(File.join("~/Library/Logs/JetBrains/*/"))
      end
    end
  end
end
