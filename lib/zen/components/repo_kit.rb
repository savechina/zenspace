# typed:true
# frozen_string_literal: true

require "yaml"
require "pathname"
require "git"

module Zen
  module Components
    class RepoKit
      include Import["logger", "settings"]

      # CodeReop root path
      attr_accessor :root
      # code workspace
      attr_accessor :workspace

      # def initialize
      #   @root = "~/CodeRepo"
      #   @workspace = "ownspace"
      # end

      # def changespace(_worksapce)
      #   @workspace = _worksapce
      # end

      # def fetch(_respository)
      #   reponame = "zen"
      #   reponame = _respository unless _respository.nil?

      #   path = Pathname.new(@root).join(@workspace).join(reponame)
      #   git = Git.open(path)
      #   puts git.show
      #   puts git.fetch
      # end

      require "date"

      # Code root path default is CodeRepo
      REPO_ROOT = File.join(Dir.home, "CodeRepo")

      # coderepo code space
      def space(space_name, group_name, project_name)
        puts "Env:"
        puts "Root:#{REPO_ROOT}"

        puts "Space:#{space_name},Group:#{group_name}"

        group_path = if group_name.nil?
                       File.join(REPO_ROOT, space_name)
                     else
                       File.join(REPO_ROOT, space_name, group_name)
                     end

        puts "GroupPath:#{group_path}"

        #  格式化输出工程列表
        puts ""
        puts "Projects:"

        menu_options = {}

        selected = []

        cache_project = JSON.parse(ENV.fetch("repo.session.projects", "[]"))

        # 过滤子目录，返回Project 列表
        subdirs = Dir.entries("#{group_path}")
                     .select do |entry|
                    File.directory?(File.join(group_path,
                                              entry)) and (project_name.nil? or entry.include?(project_name))
                  end
                     .reject { |entry| entry.start_with?(".") || entry == ".." }

        # 定义菜单选项,构建Project列表
        subdirs.sort!.each.with_index do |path, index|
          project_path = File.join(group_path, path)
          menu_options.store("#{index}", { name: path, path: project_path })
        end

        # session group
        session_group = { group_name: group_name, group_path: group_path }

        require "readline"

        # 定义菜单选项
        menu_options.store("s", { name: "Show Selected." })
        menu_options.store("a", { name: "Append Selected." })
        menu_options.store("w", { name: "Write Selected." })
        menu_options.store("q", { name: "Quit" })

        loop do
          # 显示菜单
          puts "Please choose an option:"
          menu_options.each { |key, value| puts "#{key}. #{value[:name]}" }

          # 获取用户选择
          choice = Readline.readline("Your choice: ", true)

          # 处理用户选择
          if choice.match?(/^\d+$/) && (0..menu_options.size - 4).cover?(choice.to_i)
            # choice_index = choice.to_i - 1
            # puts "你选择了: #{menu_options[choice_index]}"
            project = menu_options[choice]

            puts "You chose Project: #{choice}, Project:#{project[:name]}, Path:#{project[:path]} "
            puts ""

            selected << project
            # 删除重复添加的
            selected.uniq!

          # puts "#{selected}"

          elsif choice == "s"

            puts "Selected: "
            # selected.each do |ietm|
            #   puts " #{ietm[:name]}"
            # end
            selected_show selected

          elsif choice == "a"

            puts "Selected: "
            selected_show selected

            puts "Append..."

            selected_project = selected + cache_project
            selected_project.uniq!

            session_project space_name, session_group, selected_project

            break

          elsif choice == "w"

            puts "Selected: "

            selected_show selected

            puts "Write..."

            selected_project = selected

            session_project space_name, session_group, selected_project

            break

          elsif choice == "q"

            puts "Exiting..."
            break
          else
            puts "Invalid choice, please try again."
          end

          puts ""
        end
        # return selected project
        selected
      end

      # 获取工程项目最新代码
      def fetch(_space_name, _group_name, _project_name)
        session_space = ENV.fetch("repo.session.space", nil)
        session_group = JSON.parse(ENV.fetch("repo.session.group", "{}"))["group_name"]

        puts "ENV:\n space:#{session_space} \n group:#{session_group}"
        # puts session_space
        # puts session_group

        session_project = JSON.parse(ENV.fetch("repo.session.projects", "[]"))
        puts "Projects:"

        # session_project.each do |project|
        #   puts " #{project['name']}"
        # end
        selected_show(session_project)

        puts "Fetch ALL:"

        session_project.each do |project|
          name = project["name"]
          path = project["path"]
          puts "fetch #{name} ..."

          Dir.chdir(path)
          system("git fetch ")
          system("git pull ")
        end
      end

      # 工程特性分支创建及分支批量切换
      def feature(feature_name, version_name, next_version, branch, checkout, feature)
        now = Date.today

        now_date = now.strftime("%Y%m%d")

        version_name = version(next_version) if version_name.nil?

        puts "version: #{version_name}"

        # 动态生成当前版本的分支名称
        branch_name = "feature_#{version_name}_#{feature_name}_#{now_date}"
        # 若指定分支名称，使用指定的分支名称
        branch_name = feature_name if feature

        puts "feature_name:\n #{branch_name}"

        session_project = JSON.parse(ENV.fetch("repo.session.projects", "[]"))
        puts "Projects:"
        # puts session_project

        selected_show(session_project)
        # session_project.each do |project|
        #   puts " #{project['name']}"
        # end

        if checkout
          session_project.each do |project|
            name = project["name"]
            path = project["path"]
            puts "create branch #{name} ..."

            Dir.chdir(path)
            system("git checkout master")
            system("git pull")
            system("git checkout #{branch_name}")
          end
          return
        end

        return unless branch

        session_project.each do |project|
          name = project["name"]
          path = project["path"]
          puts "create branch #{name} ..."

          Dir.chdir(path)
          system("git checkout master")
          system("git pull")
          system("git checkout -b #{branch_name}")
        end
      end

      private

      #  Show ProjectList
      def selected_show(selected)
        selected.each do |ietm|
          # ietm = ietm.with_indifferent_access
          name = ietm["name"] || ietm[:name]
          # name = ietm[:name]
          puts " #{name}"
        end
      end

      # Repo 工作空间及工作项目工程切换存储缓存配置
      def session_project(space_name, session_group, session_project)
        # puts "#{space_name},#{session_group},#{session_project}"

        save_config("repo.session.space", space_name)
        save_config("repo.session.group", session_group.to_json)
        save_config("repo.session.projects", session_project.to_json)
      end

      # 追加或更新环境变量
      def save_config(key, value)
        config_file = File.join(Zen::USER_ROOT, "config", ".zen")

        config_path = Pathname(config_file).parent

        FileUtils.mkdir_p(config_path) unless File.exist?(config_path)

        # 读取 .env 文件内容
        if File.exist?(config_file)
        lines = File.readlines(config_file)
        else
        lines=[]
        end

        # 检查键是否已存在
        key_exists = lines.any? { |line| line.start_with?("#{key}=") }

        # 更新或追加记录
        if key_exists
          lines.map! do |line|
            line.start_with?("#{key}=") ? "#{key}=#{value}" : line
          end
        else
          lines << "#{key}=#{value}\n"
        end

        # 将更新后的内容写回 .env 文件
        File.open(config_file, "w") do |file|
          file.puts lines
        end
      end

      # 获取当前版本号
      def version(next_version)
        # 当月最后一个星期四,或者最后一天
        current = version_date(next_version)
        puts "version_date:#{current}"

        version_date = current.strftime("%y.%m%d")

        "v#{version_date}"
      end

      # 每月最后一天
      def last_day_of_month(date = Date.today)
        next_month = Date.new(date.year, date.month, 1).next_month
        next_month - 1
      end

      # 版本日期，
      # 版本规则：取每月最后一天且最后一天不为星期天、星期六，否则则取每月最后一个星期四。
      def version_date(next_version)
        # 当前月末最后一天
        last_day = Date.today.end_of_month
        last_day = Date.today.next_month.end_of_month if next_version

        # 计算最后一个星期四
        if [0, 6].include?(last_day.wday) # 0: 星期日, 6: 星期六
          # 从最后一天开始，向前查找星期四（wday=4）
          7.times do |days_ago|
            candidate = last_day - days_ago.days
            return candidate if candidate.wday == 4
          end
        else
          last_day # 直接返回当月最后一天
        end
      end

    end
  end
end
