use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum WpsCommands {
    /// Adds two numbers
    Add {
        /// The first number
        a: i32,
        /// The second number
        b: i32,
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
        WpsCommands::Add { a, b } => {
            println!("{} + {} = {}", a, b, a + b);
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
