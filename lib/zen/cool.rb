require 'thor'

require 'thor/rake_compat'
require "rake/tasklib"

class RakeTask < Rake::TaskLib
  def initialize
    define
  end

  def define
    instance_eval do
      desc "Say it's cool"
      task :cool do
        puts "COOL"
      end

      namespace :hiper_mega do
        task :super do
          puts "HIPER MEGA SUPER"
        end
      end
    end
  end
end

class ThorTask < Thor
  include Thor::RakeCompat
  RakeTask.new
  
  desc 'cool', 'say cool'
  def cool
    Rake::Task['cool'].invoke
    puts ThorTask.tasks['cool'].description
  end
end

# ThorTask.start(ARGV)