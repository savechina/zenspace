use std::fs;
use std::process::{Command, Stdio};

use crate::utils;

pub fn develop_tool() {
    println!("Develop initialization: ");

    let exists = utils::command_exists("cargo");

    println!("rustc exists: {}", exists);

    let java_version = "openjdk@11";

    let tools = vec![
        java_version,
        "jenv",
        "rbenv",
        "zstd",
        "maven",
        "intellij-idea",
        "visual-studio-code",
        "iterm2",
        "dbeaver-community",
        "cloc",
        "octosql",
        "tree",
        "sqlite",
        "uv",
        "zed",
        "helix",
        "bat",
        "zoxide",
        "fzf",
        "ripgrep",
    ];

    for tool in tools {
        println!("Start to install {}...", tool);

        let status = Command::new("brew")
            .arg("install")
            .arg(tool)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .unwrap();

        if !status.success() {
            eprintln!("Failed to install {}", tool);
        }
    }

    println!("Configuring jenv to add OpenJDK...");

    let java_home = format!(
        "/opt/homebrew/opt/{}/libexec/openjdk.jdk/Contents/Home",
        java_version
    );

    let status = Command::new("jenv")
        .arg("add")
        .arg(java_home)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .unwrap();

    if !status.success() {
        eprintln!("Failed to configure jenv");
    }
}

pub fn workspace() {
    println!("Workspace initialization:");

    let home = home::home_dir().expect("Could not get home directory");

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

    for file in file_list {
        if file.exists() {
            println!("{} exists", file.display());
        } else {
            fs::create_dir_all(&file).unwrap();
            println!("Created directory: {}", file.display());
        }
        println!("workspace init: {} done", file.display());
    }

    let export_path = home.join("export");
    #[cfg(unix)]
    {
        Command::new("ln")
            .arg("-s")
            .arg(&export_path)
            .arg("/export")
            .status()
            .unwrap();
    }
}
