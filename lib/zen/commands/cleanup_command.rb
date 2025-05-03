# typed: true
# frozen_string_literal: true

module Zen
  module Commands
    ##
    # CleanCommand for cleanup macos logs and cache Utils
    #
    class CleanupCommand < Zen::Commands::BaseCommand
      include Import["zen.components.cleanup_kit", "logger"] # => [Zen::Components::RepoKit]

      desc "all ", "fetch NAME repository"
      def all
        logger.info("entry repo clean ")
        puts "cleanup start"
        cleanup_kit.clean_all
      end

      default_command :all

      ##
      #  base define
      self.command_name = "cleanup"
      self.command_usage = "cleanup "
      self.command_description = "cleanup macos logs and cache Utils"
    end
  end
end
