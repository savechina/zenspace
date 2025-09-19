use crate::infra::starter_repository;
use crate::model::starter_model::{JavaTypeMapping, JavaTypes, Project};
use crate::util;
use heck::{self, ToUpperCamelCase};
use std::env::{self, home_dir};
use std::fs::DirEntry;
use std::fs::{self, FileType};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use strum::IntoEnumIterator;

pub(crate) fn init(project: Project, output_root: PathBuf) {
    println!("{} ", "Develop initialize:");

    // let template_path = util::TEMPLATES.path();
    // println!("template path: {:?}", template_path);

    // // 遍历所有文件（包括子目录中的文件）
    // for file in util::TEMPLATES.dirs() {
    //     println!("Recursive File path: {:?}", file.path());

    //     file.dirs().for_each(|f| {
    //         println!("Recursive File path: {:?}", f.path());
    //     });

    //     file.files().for_each(|f| {
    //         println!("Recursive File path: {:?}", f.path());
    //     });
    // }

    // let template_name = "templates";

    // let template_path = PathBuf::from(template_name);

    // if template_path.exists() {
    //     println!("template path exists: {}", true);
    // }

    // let ddd_path = template_path.join("starter/ddd_init");

    // println!("ddd_path exists: {} ", ddd_path.exists());

    // let ddd_dir = fs::read_dir(ddd_path).unwrap();

    // for entry in ddd_dir {
    //     let entry: DirEntry = entry.unwrap();
    //     let file_name = entry.file_name();
    //     let file_type: FileType = entry.file_type().unwrap();
    //     let path = entry.path();

    //     println!(
    //         "file name: {:?},type: {:?} ,path: {:?}",
    //         file_name, file_type, path
    //     );
    // }
    //

    let arch_type = &project.arch_type;
    let template_base = if arch_type.eq_ignore_ascii_case("ddd") {
        "starter/ddd_init"
    } else if arch_type.eq_ignore_ascii_case("mvc") {
        "starter/mvc_init"
    } else {
        //default ddd
        "starter/ddd_init"
    };

    //get template entry from TEMPLATES
    let template_entry = util::TEMPLATES.get_entry(template_base).unwrap();

    process_entry(
        template_entry,
        0,
        project,
        template_base.to_string(),
        output_root,
    );
}

#[tokio::main]
pub(crate) async fn add() {
    println!("{} ", "Develop initialize:");

    let char = JavaTypes::Char.info();

    let column_type = char.db_type.to_upper_camel_case();
    println!("Char column_type:{}", column_type);

    // "Char".make_ascii_uppercase();
    let c = JavaTypes::from_str("Char").unwrap();
    println!("JavaTypes: {:?}", c.info());

    for t in JavaTypes::iter() {
        println!("JavaTypes: {:?}", t.info());
    }

    println!("fetch field ...");
    let table_name = "qms_monitor_data";

    starter_repository::fetch_clazz(table_name.to_ascii_uppercase()).await;

    starter_repository::fetch_field(table_name.to_ascii_uppercase())
        .await
        .unwrap();
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

/// process template entry
fn process_entry(
    entry: &include_dir::DirEntry,
    depth: usize,
    project: Project,
    template_base: String,
    output_root: PathBuf,
) {
    let indent = "  ".repeat(depth);
    let base = template_base.clone();

    match entry {
        include_dir::DirEntry::Dir(dir) => {
            //template path
            let tpl_path = dir.path().strip_prefix(base).unwrap();

            //target path
            let target_path = handle_target_path(tpl_path, &project);

            //output full path
            let output_path = output_root.join(target_path.clone());

            println!(
                "{}D tpl: {}, target: {} , exists: {}",
                indent,
                dir.path().display(),
                target_path,
                output_path.exists()
            );

            if !output_path.exists() {
                fs::create_dir_all(output_path).expect("create target directory is error");
            }

            for subentry in dir.entries() {
                process_entry(
                    subentry,
                    depth + 1,
                    project.clone(),
                    template_base.clone(),
                    output_root.clone(),
                );
            }
        }

        include_dir::DirEntry::File(file) => {
            let tpl_path = file.path().strip_prefix(base).unwrap();

            let file_name = tpl_path.file_name();

            let target_path = handle_target_path(tpl_path, &project);
            let output_path = output_root.join(target_path.clone());

            println!(
                "{}F tpl: {}, target: {} , exists: {}",
                indent,
                file.path().display(),
                target_path,
                output_path.exists()
            );

            if let Some(content) = file.contents_utf8() {
                //parse template context ,and get target context
                let target_content = handle_target_context(project, content);

                //write target context to file
                std::fs::write(output_path, target_content).expect("write target file is error");

                println!("{}C  内容: {}", indent, content.len());
            } else {
                println!("{}C  字节数: {}", indent, file.contents().len());
            }
        }
    }
}

fn handle_target_context(project: Project, content: &str) -> String {
    let project_name = &project.project_name;
    let package_name = &project.package_name;
    let group_name = &project.group_name;

    //handle template output target context
    let target_content = content
        .replace("__app__", project_name)
        .replace("__package__", package_name)
        .replace("__group__", group_name);
    target_content
}

fn handle_target_path(source_path: &Path, project: &Project) -> String {
    let project_name = project.project_name.clone();
    let package_name = project.package_name.clone();
    let group_name = project.group_name.clone();

    let target_path = source_path
        .to_string_lossy()
        .replace("__app__", project_name.as_str())
        .replace(
            "__package__",
            package_name.to_string().replace(".", "/").as_str(),
        );

    target_path
}
