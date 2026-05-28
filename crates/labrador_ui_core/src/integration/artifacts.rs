use std::path::{Path, PathBuf};

pub const ARTIFACTS_KEY: &str = "test_artifacts";

pub const ARTIFACTS_DIR_ENV_VAR: &str = "LABRADOR_INTEGRATION_TEST_ARTIFACTS_DIR";

pub fn artifacts_root_dir() -> PathBuf {
    super::env_var(ARTIFACTS_DIR_ENV_VAR)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("labrador_integration_test_artifacts"))
}

pub struct TestArtifacts {
    dir: PathBuf,
}

impl TestArtifacts {
    pub fn new(test_name: &str) -> Self {
        let root = artifacts_root_dir();
        let timestamp = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
        let dir = root.join(test_name).join(timestamp);
        std::fs::create_dir_all(&dir).ok();
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, filename: &str) -> PathBuf {
        self.dir.join(filename)
    }
}

pub fn get_artifacts(step_data_map: &super::step::StepDataMap) -> Option<&TestArtifacts> {
    step_data_map.get::<_, TestArtifacts>(ARTIFACTS_KEY)
}
