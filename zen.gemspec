# frozen_string_literal: true

require_relative "lib/zen/version"

Gem::Specification.new do |spec|
  spec.name          = "zen"
  spec.version       = Zen::VERSION
  spec.authors       = ["RenYan Wei"]
  spec.email         = ["weirenyan@hotmail.com"]

  spec.summary       = "Write a short summary, because RubyGems requires one."
  spec.description   = "Write a longer description or delete this line."
  spec.homepage      = "http://savechina.github.io/zenspace"
  spec.license       = "MIT"
  spec.required_ruby_version = Gem::Requirement.new(">= 3.2.0")

  # spec.metadata["allowed_push_host"] = "http://renyan.org'"

  spec.metadata["homepage_uri"] = spec.homepage
  spec.metadata["source_code_uri"] = "https://github.com/savechina/zenspace.git"
  spec.metadata["changelog_uri"] = "https://github.com/savechina/zenspace/blob/main/CHANGELOG.md"

  # Specify which files should be added to the gem when it is released.
  # The `git ls-files -z` loads the files in the RubyGem that have been added into git.
  spec.files = Dir.chdir(File.expand_path(__dir__)) do
    `git ls-files -z`.split("\x0").reject { |f| f.match(%r{\A(?:test|spec|features|script)/}) }
  end

  spec.bindir        = "exe"
  spec.executables   = spec.files.grep(%r{\Aexe/}) { |f| File.basename(f) }
  spec.require_paths = ["lib"]

  # Uncomment to register a new dependency of your gem
  spec.add_dependency "activesupport", "~> 7.1.2"
  spec.add_dependency "config", "~> 4.0"
  spec.add_dependency "dotenv", "~> 3.1"
  spec.add_dependency "dry-events", "~> 1.0"
  spec.add_dependency "dry-monitor", "~> 1.0"
  spec.add_dependency "dry-struct", "~> 1.6"
  spec.add_dependency "dry-system", "~> 1.0"
  spec.add_dependency "factory_bot", "~> 6.2"
  spec.add_dependency "git", ">= 1.13.0", "< 2.1.0"
  spec.add_dependency "ruby-enum", "~> 1.0"
  spec.add_dependency "ruby-mysql", "~> 4.1"
  spec.add_dependency "sequel", "~> 5.54"
  spec.add_dependency "sqlite3", ">= 1.4.2"
  spec.add_dependency "thor", "~> 1.1"
  # spec.add_dependency "tty-prompt", "~> 0.23.0"

  # For more information and examples about making a new gem, checkout our
  # guide at: https://bundler.io/guides/creating_gem.html
  spec.metadata["rubygems_mfa_required"] = "true"
end
