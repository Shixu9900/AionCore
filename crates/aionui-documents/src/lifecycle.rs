use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use aionui_common::{CommandSpec, EnvVar};
use aionui_process::{FileRegistryStore, RealSpawner, Spawner, run_startup_reap};
use tokio::sync::Mutex;
use uuid::Uuid;

const MODEL_DIR: &str = "/Users/9900macbook/.mineru/models";
const GGUF_REPO: &str = "MinerU2.5-Pro-2605-1.2B-GGUF";
const GGUF: &str = "MinerU2.5-Pro-2605-1.2B-Q8_0.gguf";
const MMPROJ: &str = "mmproj-MinerU2.5-Pro-2605-1.2B-Q8_0.gguf";

/// `managed` makes this doclib start its own parse server, which runs the vision
/// model in process. Left off, doclib probes the default port, where another
/// MinerU on the machine may answer with an engine of its own. The remote URL
/// cannot be empty; a closed local port keeps the health probe off the network.
const DOCLIB_SETTINGS: [(&str, &str); 2] = [
    ("parse_server.local.mode", "managed"),
    ("parse_server.remote.url", "http://127.0.0.1:9"),
];

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

    /// MinerU resolves the GGUF pair under `base_dir/<repo name>/`, not directly
    /// under `base_dir`.
    pub fn model_ready(&self) -> bool {
        let repo = self.model_dir.join(GGUF_REPO);
        repo.join(GGUF).is_file() && repo.join(MMPROJ).is_file()
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
        let running = self
            .child
            .lock()
            .await
            .as_ref()
            .is_some_and(|child| child.pid() > 0 && !child.has_exited());
        if !running {
            self.reap_orphans().await;
            self.config.write().map_err(|e| e.to_string())?;
            if !self.config.model_ready() {
                return Err(format!(
                    "MinerU's vision model is not on disk. Place {GGUF} and {MMPROJ} in {} and parse again. The assistant will not call a cloud engine.",
                    self.config.model_dir.join(GGUF_REPO).display()
                ));
            }
            self.spawn_doclib().await?;
            if self.apply_doclib_settings().await? {
                self.stop().await;
                self.spawn_doclib().await?;
            }
        }
        self.wait_until_serving().await
    }

    /// Writes the settings doclib keeps in its own database rather than the
    /// YAML. They are read at startup, so `true` means doclib must restart.
    async fn apply_doclib_settings(&self) -> Result<bool, String> {
        self.wait_for_socket().await?;
        let socket = &self.config.socket;
        let mut changed = false;
        for (key, value) in DOCLIB_SETTINGS {
            let current = crate::provider::local_config(socket, key)
                .await
                .map_err(|e| e.to_string())?;
            if current != *value {
                crate::provider::local_set_config(socket, key, value)
                    .await
                    .map_err(|e| e.to_string())?;
                changed = true;
            }
        }
        Ok(changed)
    }

    async fn spawn_doclib(&self) -> Result<(), String> {
        if self.config.socket.exists() {
            let _ = std::fs::remove_file(&self.config.socket);
        }
        let command = which_mineru().ok_or_else(|| "mineru is not on PATH".to_string())?;
        // `mineru server` only prints help, and `mineru server start` daemonizes,
        // which would exit before the supervisor could track it. The module is
        // the foreground process both of those end up running.
        let python = python_beside(&command)
            .ok_or_else(|| format!("mineru's own Python was not found next to {}", command.display()))?;
        let spec = CommandSpec {
            command: python,
            args: vec!["-m".into(), "mineru.doclib.app".into()],
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
            // The parse server locks its working directory. Another MinerU on the
            // machine holds its own; sharing one makes ours exit at startup.
            cwd: Some(self.config.home.to_string_lossy().into_owned()),
        };
        let registry = Arc::new(FileRegistryStore::new(&self.data_dir));
        let machine = aionui_process::local_machine_id(&self.data_dir);
        let spawner = RealSpawner::new(registry, Uuid::now_v7(), machine);
        let child = spawner.spawn(spec, &[], "mineru").await.map_err(|e| e.to_string())?;
        *self.child.lock().await = Some(child);
        Ok(())
    }

    async fn wait_for_socket(&self) -> Result<(), String> {
        for _ in 0..60 {
            if self.config.socket.exists()
                && crate::provider::local_server_status(&self.config.socket).await.is_ok()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Err(format!(
            "MinerU did not answer on {} within 60s. The assistant will not send pages to the cloud.",
            self.config.socket.display()
        ))
    }

    /// Healthy alone is not enough: a doclib without its own parse server probes
    /// the default port, and another MinerU on this machine answers there with
    /// its own engine. Ready means the parse server this doclib spawned is up.
    async fn wait_until_serving(&self) -> Result<(), String> {
        for _ in 0..300 {
            if self.config.socket.exists()
                && let Ok(body) = crate::provider::local_server_status(&self.config.socket).await
            {
                let local = &body["parse_server"]["local"];
                if local["healthy"].as_bool() == Some(true) && local["managed_running"].as_bool() == Some(true) {
                    return Ok(());
                }
                let restarts = local["restart_count"].as_i64().unwrap_or(0);
                let max = local["max_restart_attempts"].as_i64().unwrap_or(3);
                if max > 0 && restarts >= max {
                    let why = local["error_msg"].as_str().unwrap_or("it stopped retrying");
                    return Err(format!(
                        "MinerU's parse server did not become ready: {why}. The assistant will not send pages to the cloud."
                    ));
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Err(format!(
            "MinerU's parse server was not ready after 300s (socket {}). The assistant will not send pages to the cloud.",
            self.config.socket.display()
        ))
    }

    pub async fn stop(&self) {
        if let Some(child) = self.child.lock().await.take() {
            let _ = child.kill(Duration::from_secs(2)).await;
        }
    }
}

/// The `mineru` launcher and the interpreter that can run `mineru.doclib.app`
/// live in the same directory. Resolve the symlink first, so a launcher in
/// `~/.local/bin` still points at the tool's own Python.
fn python_beside(mineru: &Path) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(mineru).unwrap_or_else(|_| mineru.to_path_buf());
    let python = resolved.with_file_name("python");
    python.is_file().then_some(python)
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
        assert!(yaml.contains("engine: llama-cpp"));
        assert!(yaml.contains("server_url: \"\""));
        assert!(yaml.contains("tcp:\n    enabled: false"));
        assert!(yaml.contains("title_leveling: false"));
        assert!(yaml.contains(&format!("path: {}", cfg.socket.display())));
        assert!(cfg.socket.ends_with("doclib.sock"));
    }

    #[test]
    fn the_vision_model_is_looked_up_inside_its_repo_directory() {
        let dir = std::env::temp_dir().join(format!("aionui-mineru-test-{}", Uuid::new_v4()));
        let cfg = MinerUConfig {
            home: dir.clone(),
            config_path: dir.join("config.yaml"),
            socket: dir.join("doclib.sock"),
            model_dir: dir.join("models"),
        };
        assert!(!cfg.model_ready(), "missing weights must not count as ready");

        let repo = cfg.model_dir.join(GGUF_REPO);
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join(GGUF), b"gguf").unwrap();
        std::fs::write(repo.join(MMPROJ), b"mmproj").unwrap();
        assert!(cfg.model_ready());

        // Weights dropped directly in the model dir, where the old check looked,
        // are not where MinerU loads them from.
        std::fs::write(cfg.model_dir.join(GGUF), b"gguf").unwrap();
        std::fs::remove_dir_all(&repo).unwrap();
        assert!(!cfg.model_ready());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
