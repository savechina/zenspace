# typed: true
# frozen_string_literal: true

module Zen
  module Commands
    ##
    # RepoCommand for repo Utils
    #
    class RepoCommand < Zen::Commands::BaseCommand
      # @return [Zen::Components::RepoKit]
      include Import["zen.components.repo_kit","logger"] # => [Zen::Components::RepoKit]

      ##
      # define command

      # desc "fetch <repository> [<refspec>...]", "Download objects and refs from another repository"
      # options all: :boolean, multiple: :boolean
      # option :append, type: :boolean, aliases: :a
      # def fetch(respository, *_refspec)
      #   # implement git fetch here

      #   # repo_kit

      #   # @type [Zen::Components::RepoKit]
      #   r = repo_kit
      #   r.fetch respository
      # end

      desc "fetch NAME", "fetch NAME repository"
      def fetch(space_name = nil, group_name = nil, project_name = nil)
        space_name = "#{space_name}space" unless !space_name.nil? && space_name.end_with?("space")
        repo_kit.fetch(space_name, group_name, project_name)
      end

      desc "space SPACENAME [GROUP] [PROJECT]", "use code space SPACENAME GROUP,PROJECT in repository."
      def space(space_name, group_name = nil, project_name = nil)
        logger.info("entry repo code space ")
        logger.info("space:#{space_name},group:#{group_name}")

        space_name = "#{space_name}space" unless space_name.end_with?("space")

        logger.info("space:#{space_name},group:#{group_name}")

        repo_kit.space(space_name, group_name, project_name)
      end

      desc "feature NAME", "feature NAME repository"
      option :branch, type: :boolean, aliases: "-b", desc: "Create Branch"
      option :next, type: :boolean, aliases: "-n", desc: "Next Version"
      option :checkout, type: :boolean, aliases: "-c", desc: "Checkout Branch from give feature name"
      option :featrue, type: :boolean, aliases: "-f", desc: "Give Branch"
      def feature(feature_name, version_name = nil)
        # 是否指定下一个版本
        next_version = options[:next]

        # 是否创建新分支
        branch = options[:branch]

        # 是否切换分支
        checkout = options[:checkout]
        # 是否指定分支名称
        featrue = options[:featrue]

        # logger.info("init vcl command")
        repo_kit.feature(feature_name, version_name, next_version, branch, checkout, featrue)
        # exec "git fetch #{repo_name}"
      end

      ##
      #  base define
      self.command_name = "repo"
      self.command_usage = "repo SUBCOMMAND ...ARGS"
      self.command_description = "manage set of tracked repositories"
    end
  end
end
