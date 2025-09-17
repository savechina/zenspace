#[derive(Debug, Clone)]
pub(crate) struct Project {
    ///Project Name
    pub(crate) project_name: String,
    ///Group Name
    pub(crate) group_name: String,
    /// project package name
    pub(crate) package_name: String,
    /// project arch type,value : ddd and mvc
    pub(crate) arch_type: String,
}
