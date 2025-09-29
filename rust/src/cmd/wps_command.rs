use clap::Subcommand;

use crate::service::wps_service;

#[derive(Subcommand)]
pub(crate) enum WpsCommands {
    /// Archive workspace directory.
    /// use `tar` and `zstd` command compression file to Archive driectory.
    Archive {
        /// The DIRECTORY
        from_dir: Option<String>,
    },
    /// Dotfiles backup and restore.
    #[command(visible_aliases = ["dot"] )]
    Dotfiles {
        #[arg(short, long, default_value = "false")]
        restore: bool,
    },
    /// The unixtime converter.
    /// The starting point for Unix Time is January 1, 1970, at 00:00:00 UTC.
    #[command(visible_aliases = ["time","t","timestamp"] )]
    Unixtime {
        /// The TIMESTAMP
        timestamp: Option<i64>,
        /// TIMESTAMP timeunit. :s, :ms ,:us ,:ns
        #[arg(short = 't', long, default_value = "s")]
        timeunit: String,
    },
}

///执行 clac command run
pub(crate) fn excute_command(operation: &WpsCommands) {
    match operation {
        WpsCommands::Archive {
            from_dir,
            // output_file,
        } => {
            println!(
                "zstd compress directory: {:}  ",
                from_dir.clone().unwrap_or(String::new())
            );

            wps_service::archive(from_dir.clone(), None).unwrap();
        }
        WpsCommands::Dotfiles { restore } => {
            println!("dotfiles restore: {}", restore);
            wps_service::dotfiles(restore.clone()).unwrap();
        }
        WpsCommands::Unixtime {
            timestamp,
            timeunit,
        } => {
            // println!("{} - {}", timestamp.clone().unwrap_or(-1), timeunit);

            wps_service::unixtime(timestamp.clone(), timeunit.clone()).unwrap();
        } // StarterCommands::Mul(Mul { a, b }) => {
          //     println!("{} * {} = {}", a, b, a * b);
          // }
          // StarterCommands::Div(s) => {
          //     println!("{} / {} = {}", s.a, s.b, s.a / s.b);
          // }
    }
}
