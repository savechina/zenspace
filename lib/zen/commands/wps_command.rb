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

      desc "fdupes DIRECTORY", "Find duplicates file in given your path"
      option :recurse, type: :boolean, aliases: "-r", desc: "enable recurse directory"
      option :filter, type: :string, aliases: "-f", desc: "filter file use regex"
      def fdupes(file_path)
        if file_path.nil? || file_path.strip.empty?
          puts "Error: You must provide a file_regex"
          return
        end

        file_filter = if options[:filter].nil? || options[:filter].strip.empty?
                        "*"
                      else
                        options[:filter]
                      end

        file_regex = if options[:recurse]
                       "#{file_path}/**/*{#{file_filter}}"
                     else
                       "#{file_path}/*{#{file_filter}}"
                     end

        file_dupes = wps_kit.find_duplicates(file_regex)

        file_dupes.each do |key, val|
          puts "\n#{key.file_hash}  #{key.block_count} :"
          val.each { |f| puts f }
        end
      end

      desc "dotfiles ", "dotfiles backup and restore."
      def dotfiles
        puts "dotfiel tool"

        wps_kit.dotfiles
      end

      # base define
      self.command_name = "wps"
      self.command_usage = "wps [COMMAND]"
      self.command_description = "work process suite"
    end
  end
end
