# Zen Command Usage


## `zen` Command: 

    ➜  zenspace git:(master) bundle exec zen
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

## `starter` command : `zen starter`

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

    ~/CodeRepo/acespace        
    ~/CodeRepo/funspace        
    ~/CodeRepo/ownspace        
    ~/CodeRepo/workspace
    ~/CodeRepo/workspace/airp
    ~/CodeRepo/workspace/bluekit
    ~/Documents/Work
    ~/Documents/Other
    ~/Documents/Personal
    ~/Documents/Archive
    ~/Software

`CodeRepo` is Source Code repoisitory, `Work` is work docuemnt . `Personal` is personal docuemnt

## `wps` command: `zen wps`

    $ zen wps
    Commands:
        zen wps dotfiles                         # dotfiles backup and restore.
        zen wps fdupes DIRECTORY                 # Find duplicates file in given your path
        zen wps help [COMMAND]                   # Describe subcommands or one specific subcommand
        zen wps zstds [DIRECTORY] [OUTPUT_FILE]  # zstd compression and decompression driectory

use `fdupes` subcommand to  Find duplicates file in given your path : `zen wps fdupes `

    $ zen wps fdupes -r  .


use `dotfiles` subcommand to backup and restore dotfiles. : `zen wps dotfiles `

    $ zen wps dotfiles 


dotfiles contain :

    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".profile",
    ".gitconfig",
    ".ssh",
    ".m2/setting.xml",
    ".rbenv/version",
    ".pyenv/version",
    ".vimrc",
    ".vim",
    ".config/gem",
    ".config/gh",
    ".config/pip"

use `zstds` subcommand to  zstd compression and decompression driectory : `zen wps zstds `

    $ zen wps zstds  dotfile dotfile.tar.zst