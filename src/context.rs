use serde::{Deserialize, Serialize};

pub mod proc;
pub mod sway;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AppContext {
    pub compositor: Option<String>,
    pub app_id: Option<String>,
    pub class: Option<String>,
    pub instance: Option<String>,
    pub title: Option<String>,
    pub pid: Option<u32>,
    pub exe: Option<String>,
    pub cwd: Option<String>,
    pub container_id: Option<String>,
}
