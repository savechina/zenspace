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

      desc "fdupes [DIRECTORY]", "Find duplicates file in given your path"
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

      desc "zstds [DIRECTORY] [OUTPUT_FILE]", "use tar and zstd compression and decompression driectory."
      def zstds(from_dir, output_file = nil)
        output_file = "#{from_dir}.tar.zst" if output_file.nil?

        puts "zstd compress directory: #{from_dir} > #{output_file}"

        wps_kit.zstds(from_dir, output_file)
      end

      desc "archive [DIRECTORY] [OUTPUT_FILE]",
           "archive workspace directory ,use tar and  zstd compression to Archive driectory."
      def archive(from_dir = nil, output_file = nil)
        output_file = "#{from_dir}.tar.zst" if output_file.nil?

        puts "zstd compress directory: #{from_dir} > #{output_file}"

        wps_kit.archive(from_dir, output_file)
      end

      desc "unixtime [TIMESTAMP] ", "UnixTime ,The starting point for Unix Time is January 1, 1970, at 00:00:00 UTC."
      option :timeunit, type: :string, aliases: "-t", default: "s", desc: "TIMESTAMP timeunit. :s, :ms ,:us ,:ns "
      def unixtime(timestamp = nil)
        timeunit = options[:timeunit]

        if timestamp.nil?
          wps_kit.unixtime(timestamp, timeunit)
        else

          if timeunit.nil? || timeunit.strip.empty?
            puts "require timeunit. "
            return
          end

          case timeunit
          when "s"
            timestamp = timestamp.to_d
          when "ms"
            timestamp = timestamp.to_d / 1_000.0
          when "us"
            timestamp = timestamp.to_d / 1_000.0 / 1_000.0
          when "ns"
            timestamp = timestamp.to_d / 1_000.0 / 1_000.0 / 1_000.0
          else
            puts " timeunit not support. timeunit:#{timeunit}"
          end

          puts "Timestamp:#{timestamp},timeunit:#{timeunit}"

          wps_kit.unixtime(timestamp, timeunit)
        end
      end

      # base define
      self.command_name = "wps"
      self.command_usage = "wps [COMMAND]"
      self.command_description = "work process suite"
    end
  end
end
