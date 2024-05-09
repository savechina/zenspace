# typed: true
# frozen_string_literal: true

module Zen
  module Commands
    ##
    # RepoCommand for repo Utils
    #
    class RepoCommand < Zen::Commands::BaseCommand
      # @return [Zen::Components::RepoKit]
      include Import["zen.components.repo_kit"] # => [Zen::Components::RepoKit]

      ##
      # define command

      desc "fetch <repository> [<refspec>...]", "Download objects and refs from another repository"
      options all: :boolean, multiple: :boolean
      option :append, type: :boolean, aliases: :a
      def fetch(respository, *_refspec)
        # implement git fetch here

        # repo_kit

        # @type [Zen::Components::RepoKit]
        r = repo_kit
        r.fetch respository
      end

      ##
      #  base define
      self.command_name = "repo"
      self.command_usage = "repo SUBCOMMAND ...ARGS"
      self.command_description = "manage set of tracked repositories"
    end
  end
end
