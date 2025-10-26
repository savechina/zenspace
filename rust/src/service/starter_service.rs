use crate::infra::starter_repository;
use crate::model::starter_model::{JavaClass, JavaModule, Project};
use crate::util;
use std::collections::HashMap;
use std::env::{self};
use std::fs::{self};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tera::{Context, Tera};

pub(crate) fn init_project(project: Project, output_root: PathBuf) {
    println!("Project initialize: ");

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
pub(crate) async fn add_feature(feature_name: String, table_name: String, project: Project) {
    //pwd current directory
    let current_dir = env::current_dir().unwrap();

    let output_root = current_dir;
    println!("Output: {}", output_root.display());

    let mut clazz_list =
        starter_repository::fetch_clazz(Some(table_name.to_ascii_uppercase())).await;

    if let Some(clazz_model) = clazz_list.first_mut() {
        clazz_model.feature_name = feature_name.clone();
        clazz_model.package_name = feature_name.clone();

        add_feature_modules(project, clazz_model.clone(), output_root);
        println!("Add feature: {} Done", table_name);
    } else {
        eprintln!("Add feature: {} table not found ", table_name);
    }
}

pub(crate) fn develop_tool() {
    println!("Develop initialize: ");

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
    content
        .replace("__app__", project_name)
        .replace("__package__", package_name)
        .replace("__group__", group_name)
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

pub(crate) fn add_feature_modules(project: Project, clazz_model: JavaClass, output_root: PathBuf) {
    //template base and feature modules
    let (template_base, mut modules) = if project.arch_type.eq_ignore_ascii_case("ddd") {
        (
            "starter/ddd_spec",
            get_ddd_feature_modules(&project, &clazz_model),
        )
    } else if project.arch_type.eq_ignore_ascii_case("mvc") {
        (
            "starter/mvc_spec",
            get_mvc_feature_modules(&project, &clazz_model),
        )
    } else {
        //default ddd
        (
            "starter/ddd_spec",
            get_ddd_feature_modules(&project, &clazz_model),
        )
    };

    // 1. 创建 Tera 实例
    let mut tera = Tera::default();
    //注册过滤器
    tera.register_filter("to_pascal_case", util::to_pascal_case_filter);
    tera.register_filter("to_lower_camel_case", util::to_lower_camel_case_filter);

    // 2. 创建 Context 对象并插入数据
    let mut context = Context::new();
    context.insert("modules", &modules);
    context.insert("project", &project);
    context.insert("model", &clazz_model);

    for (name, module) in &mut modules {
        let template_file = module.module_template.clone().unwrap();

        let output_path = PathBuf::new()
            .join(output_root.clone())
            .join(module.output_path().unwrap());

        dbg!(name);

        // 获取模板及内容
        let template_entry = util::TEMPLATES
            .get_file(format!("{}/{}", template_base, template_file))
            .unwrap();

        let template_context = template_entry.contents_utf8().unwrap();

        // 渲染处理模板内容
        let result = tera.render_str(template_context, &context);

        let output_dir = output_path.parent().unwrap();

        fs::create_dir_all(output_dir).expect("create output directroy is error");

        fs::write(output_path.clone(), result.unwrap()).expect("wirte resut context is error");

        println!("temtlate output:{} \n", output_path.display())
    }
}

fn get_ddd_feature_modules(
    project: &Project,
    clazz_model: &JavaClass,
) -> HashMap<&'static str, JavaModule> {
    let mut modules = HashMap::new();

    let mut entity_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("ENTITY".to_string()))
        .module_template(Some("entity.java.tera".to_string()))
        .module_package(Some("infrastructure.entity".to_string()))
        // .module_package_suffix(Some("impl".to_string()))
        .module_path(Some(format!("{}-infrastructure", project.project_name)))
        .module_suffix(Some("Entity".to_string()))
        .module_output(Some("Entity.java".to_string()))
        .build()
        .refresh();

    entity_module.refresh();
    modules.insert("entity", entity_module);

    let mapper_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("MAPPER".to_string()))
        .module_template(Some("dao.java.tera".to_string()))
        .module_package(Some("infrastructure.mapper".to_string()))
        // .module_package_suffix(Some("impl".to_string()))
        .module_path(Some(format!("{}-infrastructure", project.project_name)))
        .module_suffix(Some("Mapper".to_string()))
        .module_output(Some("Mapper.java".to_string()))
        .build()
        .refresh();

    modules.insert("mapper", mapper_module);

    let mapper_res_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::RESOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("MAPPER_RESOURCE".to_string()))
        .module_template(Some("mapper.xml.tera".to_string()))
        .module_package(Some("mapper".to_string()))
        .module_path(Some(format!("{}-infrastructure", project.project_name)))
        .module_suffix(Some("Mapper".to_string()))
        .module_output(Some("Mapper.xml".to_string()))
        .build()
        .refresh();

    modules.insert("mapper_resource", mapper_res_module);

    let entity_convert_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("ENTITY".to_string()))
        .module_template(Some("entity_convert.java.tera".to_string()))
        .module_package(Some("infrastructure.convert".to_string()))
        .module_path(Some(format!("{}-infrastructure", project.project_name)))
        .module_suffix(Some("EntityConvert".to_string()))
        .module_output(Some("EntityConvert.java".to_string()))
        .build()
        .refresh();
    modules.insert("entity_convert", entity_convert_module);

    let model_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("MODEL".to_string()))
        .module_template(Some("model.java.tera".to_string()))
        .module_package(Some("domain.model".to_string()))
        .module_package_suffix(None)
        .module_path(Some(format!("{}-domain", project.project_name)))
        .module_suffix(Some("DO".to_string()))
        .module_output(Some("DO.java".to_string()))
        .build()
        .refresh();

    modules.insert("model", model_module);

    let repository_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("REPOSITORY".to_string()))
        .module_template(Some("repository.java.tera".to_string()))
        .module_package(Some("domain.repository".to_string()))
        // .module_package_suffix(Some("impl".to_string()))
        .module_path(Some(format!("{}-domain", project.project_name)))
        .module_suffix(Some("Repository".to_string()))
        .module_output(Some("Repository.java".to_string()))
        .build()
        .refresh();

    modules.insert("repository", repository_module);

    let repository_impl_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("REPOSITORY_IMPL".to_string()))
        .module_template(Some("repository_impl.java.tera".to_string()))
        .module_package(Some("infrastructure.repository".to_string()))
        // .module_package_suffix(Some("impl".to_string()))
        .module_path(Some(format!("{}-infrastructure", project.project_name)))
        .module_suffix(Some("RepositoryImpl".to_string()))
        .module_output(Some("RepositoryImpl.java".to_string()))
        .build()
        .refresh();

    modules.insert("repository_impl", repository_impl_module);

    let service_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("SERVICE".to_string()))
        .module_template(Some("service.java.tera".to_string()))
        .module_package(Some("domain.service".to_string()))
        // .module_package_suffix(Some("impl".to_string()))
        .module_path(Some(format!("{}-domain", project.project_name)))
        .module_suffix(Some("Service".to_string()))
        .module_output(Some("Service.java".to_string()))
        .build()
        .refresh();

    modules.insert("service", service_module);

    let service_impl_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("SERVICE_IMPL".to_string()))
        .module_template(Some("service_impl.java.tera".to_string()))
        .module_package(Some("domain.service".to_string()))
        .module_package_suffix(Some("impl".to_string()))
        .module_path(Some(format!("{}-domain", project.project_name)))
        .module_suffix(Some("ServiceImpl".to_string()))
        .module_output(Some("ServiceImpl.java".to_string()))
        .build()
        .refresh();

    modules.insert("service_impl", service_impl_module);

    let view_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("VIEW".to_string()))
        .module_template(Some("vo.java.tera".to_string()))
        .module_package(Some("domain.vo".to_string()))
        .module_package_suffix(None)
        .module_path(Some(format!("{}-domain", project.project_name)))
        .module_suffix(Some("Vo".to_string()))
        .module_output(Some("Vo.java".to_string()))
        .build()
        .refresh();

    modules.insert("vo", view_module);
    modules
}

fn get_mvc_feature_modules(
    project: &Project,
    clazz_model: &JavaClass,
) -> HashMap<&'static str, JavaModule> {
    let mut modules = HashMap::new();

    let entity_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("ENTITY".to_string()))
        .module_template(Some("entity.java.tera".to_string()))
        .module_package(Some("domain.entity".to_string()))
        // .module_package_suffix(Some("impl".to_string()))
        .module_path(Some(format!("{}-domain", project.project_name)))
        .module_suffix(Some("Entity".to_string()))
        .module_output(Some("Entity.java".to_string()))
        .build()
        .refresh();

    modules.insert("entity", entity_module);

    let mapper_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("MAPPER".to_string()))
        .module_template(Some("dao.java.tera".to_string()))
        .module_package(Some("domain.mapper".to_string()))
        // .module_package_suffix(Some("impl".to_string()))
        .module_path(Some(format!("{}-domain", project.project_name)))
        .module_suffix(Some("Mapper".to_string()))
        .module_output(Some("Mapper.java".to_string()))
        .build()
        .refresh();

    modules.insert("mapper", mapper_module);

    let mapper_res_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::RESOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("MAPPER_RESOURCE".to_string()))
        .module_template(Some("mapper.xml.tera".to_string()))
        .module_package(Some("mapper".to_string()))
        .module_path(Some(format!("{}-domain", project.project_name)))
        .module_suffix(Some("Mapper".to_string()))
        .module_output(Some("Mapper.xml".to_string()))
        .build()
        .refresh();

    modules.insert("mapper_resource", mapper_res_module);

    let entity_convert_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("ENTITY".to_string()))
        .module_template(Some("entity_convert.java.tera".to_string()))
        .module_package(Some("domain.convert".to_string()))
        .module_path(Some(format!("{}-domain", project.project_name)))
        .module_suffix(Some("EntityConvert".to_string()))
        .module_output(Some("EntityConvert.java".to_string()))
        .build()
        .refresh();
    modules.insert("entity_convert", entity_convert_module);

    let model_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("MODEL".to_string()))
        .module_template(Some("model.java.tera".to_string()))
        .module_package(Some("domain.model".to_string()))
        .module_package_suffix(None)
        .module_path(Some(format!("{}-domain", project.project_name)))
        .module_suffix(Some("DO".to_string()))
        .module_output(Some("DO.java".to_string()))
        .build()
        .refresh();

    modules.insert("model", model_module);

    let service_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("SERVICE".to_string()))
        .module_template(Some("service.java.tera".to_string()))
        .module_package(Some("domain.service".to_string()))
        // .module_package_suffix(Some("impl".to_string()))
        .module_path(Some(format!("{}-domain", project.project_name)))
        .module_suffix(Some("Service".to_string()))
        .module_output(Some("Service.java".to_string()))
        .build()
        .refresh();

    modules.insert("service", service_module);

    let service_impl_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("SERVICE_IMPL".to_string()))
        .module_template(Some("service_impl.java.tera".to_string()))
        .module_package(Some("domain.service".to_string()))
        .module_package_suffix(Some("impl".to_string()))
        .module_path(Some(format!("{}-domain", project.project_name)))
        .module_suffix(Some("ServiceImpl".to_string()))
        .module_output(Some("ServiceImpl.java".to_string()))
        .build()
        .refresh();

    modules.insert("service_impl", service_impl_module);

    let view_module = JavaModule::builder()
        .project(Some(project.clone()))
        .module_type(Some(JavaModule::SOURCE_TYPE.to_string()))
        .module_model(Some(clazz_model.clone()))
        .module_name(Some("VIEW".to_string()))
        .module_template(Some("vo.java.tera".to_string()))
        .module_package(Some("domain.vo".to_string()))
        .module_package_suffix(None)
        .module_path(Some(format!("{}-domain", project.project_name)))
        .module_suffix(Some("Vo".to_string()))
        .module_output(Some("Vo.java".to_string()))
        .build()
        .refresh();

    modules.insert("vo", view_module);
    modules
}
