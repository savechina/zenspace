# typed: true
# frozen_string_literal: true

module Zen
  module Commands
    ##
    # CleanCommand for cleanup  logs and cache Utils for MacOS
    #
    class CleanupCommand < Zen::Commands::BaseCommand
      include Import["zen.components.cleanup_kit", "logger"]

      desc "all ", "clean all ."
      def all
        puts "cleanup all start ..."
        cleanup_kit.clean_all
      end

      default_command :all

      ##
      #  base define
      self.command_name = "cleanup"
      self.command_usage = "cleanup "
      self.command_description = "cleanup logs and cache Utils for MacOS"
    end
  end
end
