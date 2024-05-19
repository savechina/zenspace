# typed: true
# frozen_string_literal: true

module Zen
  module Commands
    ##
    # WpsCommand for repo Utils
    #
    class WpsCommand < Zen::Commands::BaseCommand
      include Import["zen.components.wps_kit"
                  ]

      desc "fdupes", "Find Dupes file"
      def fdupes(file_regex)
        if file_regex.nil? || file_regex.strip.empty?
          puts "Error: You must provide a file_regex"
          return
        end

        wps_kit.fdupes(file_regex)
      end

      # base define
      self.command_name = "wps"
      self.command_usage = "wps [COMMAND]"
      self.command_description = "work process suite"
    end
  end
end
