# Zen

Zen is a productivity suite designed to help you conquer common workplace challenges with ease. It provides a set of integrated tools for managing tasks, data process, repository and development , streamlining your workflow and boosting your efficiency.

## Installation

Prepare `ruby` runtime enviroment ,and dependency utils `rbenv` etc.

    $ brew install rbenv 
    $ rbenv install 3.2.1
    $ omz plugin enable rbenv

And install from source then execute:

    $ git clone https://github.com/savechina/zenspace.git
    $ cd zenspace
    $ bundle install
    $ rake install

Or install it yourself as:

    $ gem install zen

## Usage

### Zen Command: `zen`

    ➜  zenspace git:(main) bundle exec zen
    Commands:
    zen goodbye                  # say goodbye to the world
    zen hello NAME --from=FROM   # say hello to NAME
    zen help [COMMAND]           # Describe available commands or one specific command
    zen repo SUBCOMMAND ...ARGS  # manage set of tracked repositories
    zen starter [COMMAND]        # init project from template and add new feature to prjoct
    zen version                  # Zen version
    zen wps [COMMAND]            # work process suite

    Options:
    [--verbose], [--no-verbose], [--skip-verbose]


### Stater command : `zen starter`

    $ zen starter 
    Commands:
    zen starter add                               # init project from template
    zen starter help [COMMAND]                    # Describe subcommands or one specific sub...
    zen starter init <PROJECT> <GROUP> <PACKAGE>  # init project from template by project.


use `init` subcommand to  initial project : `zen starter init `

    $ zen starter init bluekit-sample org.renyan.bluekit.sample  org.renyan.bluekit.sample

use `add` subcommand to  add new feature for project : `zen starter add `

    $ zen starter add wms putin_bills

use `workspace` subcommand to initial your workspace full directory : `zen starter workspace `

    $ zen starter workspace

it will initial some workspace directory:

    ~/CodeRepo/ownspace        
    ~/CodeRepo/workspace
    ~/CodeRepo/workspace/airp
    ~/CodeRepo/workspace/bluekit
    ~/Documents/Work
    ~/Documents/Other
    ~/Documents/Personal
    ~/Documents/Archive

`CodeRepo` is Source Code repoisitory, `Work` is work docuemnt . `Personal` is personal docuemnt. 

## Development

After checking out the repo, run `bin/setup` to install dependencies. Then, run `rake spec` to run the tests. You can also run `bin/console` for an interactive prompt that will allow you to experiment.

To install this gem onto your local machine, run `bundle exec rake install`. To release a new version, update the version number in `version.rb`, and then run `bundle exec rake release`, which will create a git tag for the version, push git commits and the created tag, and push the `.gem` file to [rubygems.org](https://rubygems.org).

## Contributing

Bug reports and pull requests are welcome on GitHub at https://github.com/savechina/zenspace. This project is intended to be a safe, welcoming space for collaboration, and contributors are expected to adhere to the [code of conduct](https://github.com/savechina/zenspace/blob/main/CODE_OF_CONDUCT.md).

## License

The gem is available as open source under the terms of the [MIT License](https://opensource.org/licenses/MIT).

## Code of Conduct

Everyone interacting in the Zen project's codebases, issue trackers, chat rooms and mailing lists is expected to follow the [code of conduct](https://github.com/savechina/zenspace/blob/main/CODE_OF_CONDUCT.md).
