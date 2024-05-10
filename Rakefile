# frozen_string_literal: true

require "rake/clean"
require "rake/phony"
require "bundler/gem_tasks"
require "rspec/core/rake_task"

RSpec::Core::RakeTask.new(:spec)

require "rubocop/rake_task"

RuboCop::RakeTask.new

task default: %i[spec rubocop]

desc "starter test project  "
task starter: ["starter:init"] do
end

namespace :starter do
  desc "starter init test project  "
  task :init do
    sh "bundle exec zen starter init bluekit-sample org.renyan.bluekit.sample  org.renyan.bluekit.sample"
  end
end
# clean build or test target file
CLEAN.include "bluekit-sample"

CLEAN.include "vendor"
