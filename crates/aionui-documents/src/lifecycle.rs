use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use aionui_common::{CommandSpec, EnvVar};
use aionui_process::{FileRegistryStore, RealSpawner, Spawner, run_startup_reap};
use tokio::sync::Mutex;
use uuid::Uuid;

const MODEL_DIR: &str = "/Users/9900macbook/.mineru/models";
const GGUF: &str = "MinerU2.5-Pro-2605-1.2B-Q8_0.gguf";
const MMPROJ: &str = "mmproj-MinerU2.5-Pro-2605-1.2B-Q8_0.gguf";

#[derive(Debug, Clone)]
pub struct MinerUConfig {
    pub home: PathBuf,
    pub config_path: PathBuf,
    pub socket: PathBuf,
    pub model_dir: PathBuf,
}

impl MinerUConfig {
    pub fn for_data_dir(data_dir: &Path) -> Self {
        let home = data_dir.join("mineru");
        Self {
            config_path: home.join("config.yaml"),
            socket: home.join("doclib.sock"),
            home,
            model_dir: PathBuf::from(MODEL_DIR),
        }
    }

    pub fn yaml(&self) -> String {
        format!(
            "model:\n  base_dir: {}\n  source: local\n  small_backend: torch\n  vlm:\n    engine: llama-cpp\n    server_url: \"\"\n    max_concurrency: 1\n    http_timeout: 600\nllm_aided:\n  features:\n    title_leveling: false\n    cross_page_table_cell_merge: false\ndoclib:\n  uds:\n    enabled: true\n    path: {}\n    permission: 384\n  tcp:\n    enabled: false\n  managed_parse_server:\n    host: 127.0.0.1\n  parse_workers: 1\n  ingest_workers: 1\n",
            self.model_dir.display(),
            self.socket.display()
        )
    }

    pub fn model_ready(&self) -> bool {
        self.model_dir.join(GGUF).is_file() && self.model_dir.join(MMPROJ).is_file()
    }

    pub fn write(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.home)?;
        std::fs::write(&self.config_path, self.yaml())?;
        Ok(())
    }
}

pub struct MinerULifecycle {
    config: MinerUConfig,
    data_dir: PathBuf,
    child: Mutex<Option<Arc<aionui_process::ManagedProcess>>>,
}

impl MinerULifecycle {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            config: MinerUConfig::for_data_dir(&data_dir),
            data_dir,
            child: Mutex::new(None),
        }
    }

    /// One instance per assistant data directory so parse routes and shutdown share it.
    pub fn shared(data_dir: PathBuf) -> Arc<Self> {
        static INSTANCES: OnceLock<StdMutex<Vec<(PathBuf, Arc<MinerULifecycle>)>>> = OnceLock::new();
        let mut list = INSTANCES
            .get_or_init(|| StdMutex::new(Vec::new()))
            .lock()
            .expect("mineru lifecycle registry");
        if let Some((_, existing)) = list.iter().find(|(path, _)| path == &data_dir) {
            return existing.clone();
        }
        let created = Arc::new(Self::new(data_dir.clone()));
        list.push((data_dir, created.clone()));
        created
    }

    pub fn config(&self) -> &MinerUConfig {
        &self.config
    }

    pub fn endpoint(&self) -> String {
        format!("unix://{}", self.config.socket.display())
    }

    pub async fn reap_orphans(&self) {
        let registry = FileRegistryStore::new(&self.data_dir);
        let _ = run_startup_reap(&registry, aionui_process::LockState::Acquired, Uuid::now_v7(), "local");
    }

    pub async fn ensure_started(&self) -> Result<(), String> {
        if let Some(child) = self.child.lock().await.as_ref()
            && child.pid() > 0
        {
            return Ok(());
        }
        self.reap_orphans().await;
        self.config.write().map_err(|e| e.to_string())?;
        if !self.config.model_ready() {
            return Err(format!(
                "MinerU 2605 GGUF is not on disk. Place {GGUF} and {MMPROJ} in {} then parse again. The assistant will not call a cloud engine.",
                self.config.model_dir.display()
            ));
        }
        if self.config.socket.exists() {
            let _ = std::fs::remove_file(&self.config.socket);
        }
        let command = which_mineru().ok_or_else(|| "mineru is not on PATH".to_string())?;
        let spec = CommandSpec {
            command,
            args: vec!["server".into()],
            env: vec![
                EnvVar {
                    name: "MINERU_HOME".into(),
                    value: self.config.home.to_string_lossy().into_owned(),
                },
                EnvVar {
                    name: "MINERU_CONFIG".into(),
                    value: self.config.config_path.to_string_lossy().into_owned(),
                },
            ],
            cwd: Some(self.config.home.to_string_lossy().into_owned()),
        };
        let registry = Arc::new(FileRegistryStore::new(&self.data_dir));
        let machine = aionui_process::local_machine_id(&self.data_dir);
        let spawner = RealSpawner::new(registry, Uuid::now_v7(), machine);
        let child = spawner
            .spawn(spec, &[], "mineru")
            .await
            .map_err(|e| e.to_string())?;
        *self.child.lock().await = Some(child);
        for _ in 0..75 {
            if self.config.socket.exists() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Err(format!(
            "MinerU started but the local socket {} never appeared. The assistant will not send pages to the cloud.",
            self.config.socket.display()
        ))
    }

    pub async fn stop(&self) {
        if let Some(child) = self.child.lock().await.take() {
            let _ = child.kill(Duration::from_secs(2)).await;
        }
    }
}

fn which_mineru() -> Option<PathBuf> {
    let home = dirs_home();
    let candidates = [
        PathBuf::from("/Users/9900macbook/.local/bin/mineru"),
        home.join(".local/bin/mineru"),
        PathBuf::from("/opt/homebrew/bin/mineru"),
        PathBuf::from("/usr/local/bin/mineru"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/Users/9900macbook"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_stays_local_and_silent_on_the_network() {
        let cfg = MinerUConfig::for_data_dir(Path::new("/tmp/aionui-test"));
        let yaml = cfg.yaml();
        assert!(yaml.contains("source: local"));
        assert!(yaml.contains("server_url: \"\""));
        assert!(yaml.contains("tcp:\n    enabled: false"));
        assert!(yaml.contains("title_leveling: false"));
        assert!(yaml.contains(&format!("path: {}", cfg.socket.display())));
        assert!(cfg.socket.ends_with("doclib.sock"));
    }
}
