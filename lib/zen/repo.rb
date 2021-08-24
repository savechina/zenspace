require "yaml"
require "pathname"
require "git"

module Zen
  class Repo
    # CodeReop root path
    attr_accessor :root
    # code workspace
    attr_accessor :workspace

    def initialize
      @root = "~/CodeRepo"
      @workspace = "ownspace"
    end

    def changespace(_worksapce)
      @workspace = _worksapce
    end

    def fetch(_respository)
      reponame = "zen"
      reponame = _respository unless _respository.nil?

      path = Pathname.new(@root).join(@workspace).join(reponame)
      git = Git.open(path)
      puts git.show
    end
  end
end
