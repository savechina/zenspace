# frozen_string_literal: true

require "fileutils"

module Zen
  ##
  # StarterKit
  #
  module StarterKit
    #
    # Scaffold 脚手架
    #
    class Scaffold
      String TEMPLATE_ROOT = "templates"

      def initialize; end

      def new; end
    end

    #
    # Java 脚手架工具
    #
    class JavaScaffold < Scaffold
      TEMPLATE_ROOT = "templates/java/"

      def initialize(project_name, project_group, package_name)
        @project_name = project_name
        @project_group = project_group
        @project_package = package_name
      end

      def generate
        templates_root = "#{TEMPLATE_ROOT}/java/"

        target_root = @project_name

        create_project_dir

        each_template_file do |source_name|
          target_name = apply_template_variables(source_name, true)

          puts "source：#{source_name} ,target: #{target_name} "

          source = File.join(TEMPLATE_ROOT, source_name)
          target = File.join(target_root, target_name)

          if Dir.exist?(source)
            FileUtils.mkdir_p(target)
          else
            content = apply_template_variables(File.read(source), false)
            File.write(target, content)
          end
        end
      end

      def create_project_dir
        puts("create:")

        FileUtils.mkdir(@project_name)
      rescue Errno::EEXIST
        puts("directory already exists: #{@project_name}")
      end

      #
      # foreach template file
      #
      def each_template_file
        root = Pathname.new(TEMPLATE_ROOT)

        Dir.glob("#{TEMPLATE_ROOT}/**/**").each do |path|
          el = Pathname.new(path)
          yield(el.relative_path_from(root).to_s)
        end
      end

      #
      # eval template variables
      #
      def apply_template_variables(str, isPath)
        package_name = if isPath
                         @project_package.gsub(".", "/")
                       else
                         @project_package
                       end
        #
        # project
        # __app__   project anme
        # __group__ maven group
        #
        str.gsub(/__app__/, @project_name)
           .gsub(/__group__/, @project_group)
           .gsub(/__package__/, package_name)
      end
    end
  end
end
