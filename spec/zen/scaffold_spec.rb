# typed: false
# frozen_string_literal: true

require "zen"
require "fileutils"

RSpec.describe Zen::Components::StarterKit::JavaScaffold do
  it "test java scaffold new" do
    project_name = "bluekit-sample"

    if Dir.exist?(project_name)
      puts "dir had exists ,should delete.. #{project_name}"
      FileUtils.rm_rf project_name

    end

    java_project = Zen::Components::StarterKit::Model::JavaProject.new
    java_project.project_name = project_name

    java_project.group_name = "org.renyan.bluekit"
    java_project.package_name = "org.renyan.bluekit.sample"
    output_root = "target"
    java_scaffold = Zen::Components::StarterKit::JavaScaffold.new(java_project.project_name, java_project.group_name,
                                                                  java_project.package_name, output_root)

    java_scaffold.generate output_root
  end
end
