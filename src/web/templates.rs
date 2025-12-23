use crate::db::{AnalysisResult, DaemonState, Repository};
use askama::Template;

#[derive(Template)]
#[template(path = "dashboard.html")]
pub struct DashboardTemplate {
    pub daemon_status: Option<DaemonState>,
    pub repository_count: usize,
    pub result_count: usize,
}

#[derive(Template)]
#[template(path = "repositories.html")]
pub struct RepositoriesTemplate {
    pub repositories: Vec<Repository>,
}

#[derive(Template)]
#[template(path = "results.html")]
pub struct ResultsTemplate {
    pub results: Vec<AnalysisResult>,
}
