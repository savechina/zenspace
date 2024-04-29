# frozen_string_literal: true

require "bundler/gem_tasks"
require "rspec/core/rake_task"

RSpec::Core::RakeTask.new(:spec)

require "rubocop/rake_task"

RuboCop::RakeTask.new

task default: %i[spec rubocop]

require "thor"

desc "run_thor"
task :run_thor do
  dynamic_name = "MyThor"
  eval("class #{dynamic_name} < Thor; end")

  MyThor.desc "hello NAME", "say hello to NAME"
  MyThor.option :name
  MyThor.define_method(:hello) do
    puts "Hello, #{options[:name]}!"
  end

  MyThor.start(["hello", "--name", "Alice"])
  
end
