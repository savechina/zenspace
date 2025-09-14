use crate::util;
use std::env::{self, home_dir};
use std::fs::DirEntry;
use std::fs::{self, FileType};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) fn init() {
    println!("{} ", "Develop initialize:");

    let template_name = "templates";

    let template_path = PathBuf::from(template_name);

    if template_path.exists() {
        println!("template path exists: {}", true);
    }

    let ddd_path = template_path.join("starter/ddd_init");

    println!("ddd_path exists: {} ", ddd_path.exists());

    let ddd_dir = fs::read_dir(ddd_path).unwrap();
    // let ddd_dir = ddd_path.read_dir().unwrap();

    for entry in ddd_dir {
        let entry: DirEntry = entry.unwrap();
        let file_name = entry.file_name();
        let file_type: FileType = entry.file_type().unwrap();
        let path = entry.path();

        println!(
            "file name: {:?},type: {:?} ,path: {:?}",
            file_name, file_type, path
        );
    }
}

pub(crate) fn add() {
    println!("{} ", "Develop initialize:");
}

pub(crate) fn develop_tool() {
    println!("{} ", "Develop initialize:");

    let exists = util::command_exists("cargo");

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

    // Install development tools using Homebrew
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

    // Configure jenv to add OpenJDK
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

pub(crate) fn workspace() {
    println!("Workspace initialize:");

    // Get the home directory
    let home = home::home_dir().expect("Could not get home directory");

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
        println!("workspace init: {} done", file.display());
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
}
