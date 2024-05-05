# frozen_string_literal: true

require "zen"
require "fileutils"

RSpec.describe Zen::ScaffoldKit::JavaScaffold do
  it "test java scaffold new" do
    project_name = "airp-scm-planning"

    if Dir.exist?(project_name)
      puts "dir had exists ,should delete.. #{project_name}"
      FileUtils.rm_rf project_name

    end
    package_name = "com.jd.airp.scm.planning"

    puts "com.jd.airp.scm.planning:#{package_name.gsub(".", "/")}"

    project_group = "com.jd.airp.scm"
    package_name = "com.jd.airp.scm.planning"
    java_scaffold = Zen::ScaffoldKit::JavaScaffold.new(project_name, project_group, package_name, nil)

    java_scaffold.generate
  end
end
