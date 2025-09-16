use clap::Subcommand;

use crate::service::wps_service;

#[derive(Subcommand)]
pub(crate) enum WpsCommands {
    /// Archive workspace directory; use tar and  zstd compression to Archive driectory.
    Archive {
        /// The DIRECTORY
        from_dir: Option<String>,
    },
    /// Subtracts two numbers
    Sub {
        /// The first number
        a: i32,
        /// The second number
        b: i32,
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
        WpsCommands::Sub { a, b } => {
            println!("{} - {} = {}", a, b, a - b);
        } // StarterCommands::Mul(Mul { a, b }) => {
          //     println!("{} * {} = {}", a, b, a * b);
          // }
          // StarterCommands::Div(s) => {
          //     println!("{} / {} = {}", s.a, s.b, s.a / s.b);
          // }
    }
}
