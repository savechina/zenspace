use std::env::{self, home_dir};
use std::fs;

pub(crate) fn init() {
    println!("{} ", "Develop initialize:");
}

pub(crate) fn add() {
    println!("{} ", "Develop initialize:");
}

pub(crate) fn develop() {
    println!("{} ", "Develop initialize:");
}

pub(crate) fn workspace() {
    println!("Workspace initialize:");

    // Get the home directory
    let home = env::home_dir().expect("Could not get home directory");

    // List of directories
    let file_list = vec![
        home.join("export"),
        home.join("CodeRepo").join("ownspace"),
        home.join("CodeRepo").join("funspace"),
        home.join("CodeRepo").join("acespace"),
        home.join("CodeRepo").join("workspace"),
        home.join("CodeRepo").join("workspace").join("airp"),
        home.join("CodeRepo").join("workspace").join("bluekit"),
        home.join("Documents").join("Work"),
        home.join("Documents").join("Other"),
        home.join("Documents").join("Personal"),
        home.join("Documents").join("Archive"),
        home.join("Software"),
    ];

    // Iterate through the list and check/create directories
    for file in file_list {
        if file.exists() {
            println!("{} exists", file.display());
        } else {
            fs::create_dir_all(&file).unwrap();
            println!("Created directory: {}", file.display());
        }
        println!("workspace init: {}", file.display());
    }

    // Create symbolic link for export
    let export_path = home.join("export");
    #[cfg(unix)]
    {
        use std::process::Command;

        Command::new("ln")
            .arg("-s")
            .arg(&export_path)
            .arg("/export")
            .status()
            .unwrap();
    }

    // Ok(())
}
