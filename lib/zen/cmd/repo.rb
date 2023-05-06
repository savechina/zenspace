require "zen/repo"
module Zen
  class RepoCommand < Thor
    desc "fetch <repository> [<refspec>...]", "Download objects and refs from another repository"
    options all: :boolean, multiple: :boolean
    option :append, type: :boolean, aliases: :a
    def fetch(respository, *_refspec)
      # implement git fetch here

      r = Repo.new
      r.fetch respository
    end
  end
end
