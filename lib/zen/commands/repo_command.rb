require "zen/components/repo"
module Zen
  module Commands
    ##
    # RepoCommand for repo Utils
    #
    class RepoCommand < Zen::Commands::BaseCommand
      include Import["zen.components.repo"]

      ##
      # define command

      desc "fetch <repository> [<refspec>...]", "Download objects and refs from another repository"
      options all: :boolean, multiple: :boolean
      option :append, type: :boolean, aliases: :a
      def fetch(respository, *_refspec)
        # implement git fetch here

        r = repo
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
