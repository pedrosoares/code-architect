//! `~/.config/code-architect/profiles.json` — one store for the whole machine.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::ConfigError,
    types::{DocsConfig, IntegrationsConfig, McpServerConfig, Profile},
};

#[derive(Debug, Default, Serialize, Deserialize)]
struct Document {
    active: Option<Uuid>,
    #[serde(default)]
    profiles: Vec<Profile>,
    #[serde(default)]
    mcp_servers: Vec<McpServerConfig>,
    #[serde(default)]
    integrations: IntegrationsConfig,
    #[serde(default)]
    docs: DocsConfig,
}

/// Saved API configurations, global to the machine rather than a workspace —
/// an API key is set up once and reused across every project.
pub struct ConfigStore {
    path: PathBuf,
    /// Serializes read-modify-write so two CRUD calls in flight at once can't
    /// clobber each other's write.
    lock: Arc<Mutex<()>>,
}

impl ConfigStore {
    /// Opens `~/.config/code-architect/profiles.json` (or under
    /// `$XDG_CONFIG_HOME` if set), creating the directory if needed.
    pub async fn open_default() -> Result<Self, ConfigError> {
        Self::open(config_dir()?.join("profiles.json")).await
    }

    /// Opens a store at an explicit path — the seam tests use to avoid
    /// touching the real machine-wide config.
    pub async fn open(path: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let path = path.into();

        if let Some(parent) = path.parent() {
            let parent = parent.to_owned();
            let created = tokio::task::spawn_blocking(move || std::fs::create_dir_all(&parent))
                .await
                .map_err(|error| ConfigError::TaskPanicked(error.to_string()))?;
            created.map_err(|source| ConfigError::Io {
                path: path.clone(),
                source,
            })?;
        }

        Ok(Self {
            path,
            lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn list(&self) -> Result<Vec<Profile>, ConfigError> {
        Ok(self.read().await?.profiles)
    }

    pub async fn active(&self) -> Result<Option<Uuid>, ConfigError> {
        Ok(self.read().await?.active)
    }

    /// Insert a new profile, or replace an existing one with the same id.
    pub async fn upsert(&self, profile: Profile) -> Result<(), ConfigError> {
        self.rewrite(move |document| {
            match document.profiles.iter_mut().find(|p| p.id == profile.id) {
                Some(existing) => *existing = profile,
                None => document.profiles.push(profile),
            }
            Ok(())
        })
        .await
    }

    /// Remove a profile. Clears `active` too if it was the one removed.
    pub async fn remove(&self, id: Uuid) -> Result<(), ConfigError> {
        self.rewrite(move |document| {
            document.profiles.retain(|p| p.id != id);
            if document.active == Some(id) {
                document.active = None;
            }
            Ok(())
        })
        .await
    }

    /// Mark a profile as the one to use. Must already exist.
    pub async fn set_active(&self, id: Uuid) -> Result<(), ConfigError> {
        self.rewrite(move |document| {
            if !document.profiles.iter().any(|p| p.id == id) {
                return Err(ConfigError::UnknownProfile(id));
            }
            document.active = Some(id);
            Ok(())
        })
        .await
    }

    /// Stop using whichever profile is active, if any. Unlike [`Self::remove`],
    /// the profile itself is untouched — this is how the settings panel's
    /// "Default" row reverts to the host's own env/default-derived provider.
    pub async fn clear_active(&self) -> Result<(), ConfigError> {
        self.rewrite(move |document| {
            document.active = None;
            Ok(())
        })
        .await
    }

    pub async fn list_mcp_servers(&self) -> Result<Vec<McpServerConfig>, ConfigError> {
        Ok(self.read().await?.mcp_servers)
    }

    /// Insert a new MCP server, or replace an existing one with the same id.
    pub async fn upsert_mcp_server(&self, server: McpServerConfig) -> Result<(), ConfigError> {
        self.rewrite(move |document| {
            match document
                .mcp_servers
                .iter_mut()
                .find(|existing| existing.id == server.id)
            {
                Some(existing) => *existing = server,
                None => document.mcp_servers.push(server),
            }
            Ok(())
        })
        .await
    }

    pub async fn remove_mcp_server(&self, id: Uuid) -> Result<(), ConfigError> {
        self.rewrite(move |document| {
            document.mcp_servers.retain(|server| server.id != id);
            Ok(())
        })
        .await
    }

    pub async fn integrations(&self) -> Result<IntegrationsConfig, ConfigError> {
        Ok(self.read().await?.integrations)
    }

    /// Replaces the saved integrations wholesale — there is only ever one
    /// `IntegrationsConfig`, so this is a plain overwrite, not an
    /// upsert/remove pair like `Profile`/`McpServerConfig` need.
    pub async fn set_integrations(
        &self,
        integrations: IntegrationsConfig,
    ) -> Result<(), ConfigError> {
        self.rewrite(move |document| {
            document.integrations = integrations;
            Ok(())
        })
        .await
    }

    pub async fn docs(&self) -> Result<DocsConfig, ConfigError> {
        Ok(self.read().await?.docs)
    }

    /// Replaces the saved docs config wholesale — there is only ever one,
    /// same as [`Self::set_integrations`].
    pub async fn set_docs(&self, docs: DocsConfig) -> Result<(), ConfigError> {
        self.rewrite(move |document| {
            document.docs = docs;
            Ok(())
        })
        .await
    }

    async fn read(&self) -> Result<Document, ConfigError> {
        let path = self.path.clone();
        let lock = self.lock.clone();

        tokio::task::spawn_blocking(move || {
            let _guard = lock.lock().expect("config store lock");
            read_document(&path)
        })
        .await
        .map_err(|error| ConfigError::TaskPanicked(error.to_string()))?
    }

    async fn rewrite(
        &self,
        f: impl FnOnce(&mut Document) -> Result<(), ConfigError> + Send + 'static,
    ) -> Result<(), ConfigError> {
        let path = self.path.clone();
        let lock = self.lock.clone();

        tokio::task::spawn_blocking(move || {
            let _guard = lock.lock().expect("config store lock");
            let mut document = read_document(&path)?;
            f(&mut document)?;
            write_document(&path, &document)
        })
        .await
        .map_err(|error| ConfigError::TaskPanicked(error.to_string()))?
    }
}

fn read_document(path: &Path) -> Result<Document, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(serde_json::from_str(&contents)?),
        // No file yet means no profiles yet, not an error — the file is
        // created lazily on first write.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Document::default()),
        Err(source) => Err(ConfigError::Io {
            path: path.to_owned(),
            source,
        }),
    }
}

fn write_document(path: &Path, document: &Document) -> Result<(), ConfigError> {
    let json = serde_json::to_string_pretty(document)?;
    std::fs::write(path, json).map_err(|source| ConfigError::Io {
        path: path.to_owned(),
        source,
    })
}

fn config_dir() -> Result<PathBuf, ConfigError> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("code-architect"));
    }
    let home = std::env::var_os("HOME").ok_or(ConfigError::NoConfigDir)?;
    Ok(PathBuf::from(home).join(".config").join("code-architect"))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn store() -> (tempfile::TempDir, ConfigStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::open(dir.path().join("profiles.json"))
            .await
            .unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn a_fresh_store_has_no_profiles() {
        let (_dir, store) = store().await;

        assert!(store.list().await.unwrap().is_empty());
        assert_eq!(store.active().await.unwrap(), None);
    }

    #[tokio::test]
    async fn adds_and_lists_a_profile() {
        let (_dir, store) = store().await;
        let profile = Profile::new("Local LM Studio", "openai", "qwen/qwen3.8-27b")
            .base_url("http://localhost:1234/v1");

        store.upsert(profile.clone()).await.unwrap();

        let profiles = store.list().await.unwrap();
        assert_eq!(profiles, [profile]);
    }

    #[tokio::test]
    async fn upserting_an_existing_id_replaces_it_rather_than_duplicating() {
        let (_dir, store) = store().await;
        let profile = Profile::new("Work key", "anthropic", "claude-opus-5");
        store.upsert(profile.clone()).await.unwrap();

        let renamed = Profile {
            name: "Renamed".into(),
            ..profile.clone()
        };
        store.upsert(renamed.clone()).await.unwrap();

        let profiles = store.list().await.unwrap();
        assert_eq!(profiles, [renamed]);
    }

    #[tokio::test]
    async fn removing_a_profile_clears_it_if_it_was_active() {
        let (_dir, store) = store().await;
        let profile = Profile::new("Temp", "openai", "m");
        store.upsert(profile.clone()).await.unwrap();
        store.set_active(profile.id).await.unwrap();

        store.remove(profile.id).await.unwrap();

        assert!(store.list().await.unwrap().is_empty());
        assert_eq!(store.active().await.unwrap(), None);
    }

    #[tokio::test]
    async fn removing_an_inactive_profile_leaves_the_active_one_alone() {
        let (_dir, store) = store().await;
        let a = Profile::new("A", "openai", "m");
        let b = Profile::new("B", "openai", "m");
        store.upsert(a.clone()).await.unwrap();
        store.upsert(b.clone()).await.unwrap();
        store.set_active(a.id).await.unwrap();

        store.remove(b.id).await.unwrap();

        assert_eq!(store.active().await.unwrap(), Some(a.id));
    }

    #[tokio::test]
    async fn activating_an_unknown_profile_is_an_error() {
        let (_dir, store) = store().await;

        let Err(error) = store.set_active(Uuid::new_v4()).await else {
            panic!("activating a profile that was never saved must fail");
        };
        assert!(matches!(error, ConfigError::UnknownProfile(_)));
    }

    #[tokio::test]
    async fn clearing_active_leaves_the_profile_saved_but_no_longer_active() {
        let (_dir, store) = store().await;
        let profile = Profile::new("Local", "openai", "m");
        store.upsert(profile.clone()).await.unwrap();
        store.set_active(profile.id).await.unwrap();

        store.clear_active().await.unwrap();

        assert_eq!(store.active().await.unwrap(), None);
        assert_eq!(store.list().await.unwrap(), [profile]);
    }

    #[tokio::test]
    async fn reopening_a_store_reads_back_what_was_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");

        let first = ConfigStore::open(&path).await.unwrap();
        let profile = Profile::new("Persisted", "openai", "m");
        first.upsert(profile.clone()).await.unwrap();
        first.set_active(profile.id).await.unwrap();
        drop(first);

        let second = ConfigStore::open(&path).await.unwrap();
        assert_eq!(second.list().await.unwrap(), std::slice::from_ref(&profile));
        assert_eq!(second.active().await.unwrap(), Some(profile.id));
    }

    #[tokio::test]
    async fn a_fresh_store_has_no_mcp_servers() {
        let (_dir, store) = store().await;

        assert!(store.list_mcp_servers().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn adds_and_lists_an_mcp_server() {
        let (_dir, store) = store().await;
        let server = McpServerConfig::stdio("Git", "uvx").args(["mcp-server-git"]);

        store.upsert_mcp_server(server.clone()).await.unwrap();

        assert_eq!(store.list_mcp_servers().await.unwrap(), [server]);
    }

    #[tokio::test]
    async fn an_http_mcp_server_round_trips_through_storage() {
        // `McpTransport::Http` is a differently-shaped JSON payload than
        // `Stdio` (a nested `transport` tag, no `command`/`args`/`env`) —
        // proves that shape actually survives a real write-then-reopen, not
        // just an in-memory equality check.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");

        let first = ConfigStore::open(&path).await.unwrap();
        let server =
            McpServerConfig::http("Remote", "https://example.com/mcp").bearer_token("secret-token");
        first.upsert_mcp_server(server.clone()).await.unwrap();
        drop(first);

        let second = ConfigStore::open(&path).await.unwrap();
        assert_eq!(second.list_mcp_servers().await.unwrap(), [server]);
    }

    #[tokio::test]
    async fn a_fresh_store_has_no_integrations() {
        let (_dir, store) = store().await;

        assert_eq!(
            store.integrations().await.unwrap(),
            IntegrationsConfig::default()
        );
    }

    #[tokio::test]
    async fn saving_integrations_round_trips_through_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");

        let first = ConfigStore::open(&path).await.unwrap();
        let integrations = IntegrationsConfig {
            github_token: Some("ghp_test".to_owned()),
            github_use_gh_cli: false,
            slack_token: Some("xoxb-test".to_owned()),
            linear_api_key: Some("lin_api_test".to_owned()),
        };
        first.set_integrations(integrations.clone()).await.unwrap();
        drop(first);

        let second = ConfigStore::open(&path).await.unwrap();
        assert_eq!(second.integrations().await.unwrap(), integrations);
    }

    #[tokio::test]
    async fn saving_integrations_again_replaces_rather_than_merges() {
        let (_dir, store) = store().await;
        store
            .set_integrations(IntegrationsConfig {
                github_token: Some("ghp_old".to_owned()),
                github_use_gh_cli: false,
                slack_token: None,
                linear_api_key: None,
            })
            .await
            .unwrap();

        store
            .set_integrations(IntegrationsConfig {
                github_token: None,
                github_use_gh_cli: false,
                slack_token: Some("xoxb-new".to_owned()),
                linear_api_key: None,
            })
            .await
            .unwrap();

        let integrations = store.integrations().await.unwrap();
        assert_eq!(
            integrations.github_token, None,
            "a full overwrite, not a merge"
        );
        assert_eq!(integrations.slack_token.as_deref(), Some("xoxb-new"));
    }

    #[tokio::test]
    async fn a_fresh_store_has_the_default_docs_config() {
        let (_dir, store) = store().await;

        assert_eq!(store.docs().await.unwrap(), DocsConfig::default());
        assert!(store.docs().await.unwrap().enabled);
    }

    #[tokio::test]
    async fn saving_docs_config_round_trips_through_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.json");

        let first = ConfigStore::open(&path).await.unwrap();
        let docs = DocsConfig {
            enabled: true,
            driver: "obsidian".to_owned(),
            vault_path: Some("/home/me/notes".to_owned()),
        };
        first.set_docs(docs.clone()).await.unwrap();
        drop(first);

        let second = ConfigStore::open(&path).await.unwrap();
        assert_eq!(second.docs().await.unwrap(), docs);
    }

    #[tokio::test]
    async fn saving_docs_config_again_replaces_rather_than_merges() {
        let (_dir, store) = store().await;
        store
            .set_docs(DocsConfig {
                enabled: true,
                driver: "obsidian".to_owned(),
                vault_path: Some("/old/path".to_owned()),
            })
            .await
            .unwrap();

        store
            .set_docs(DocsConfig {
                enabled: false,
                driver: "obsidian".to_owned(),
                vault_path: None,
            })
            .await
            .unwrap();

        let docs = store.docs().await.unwrap();
        assert!(!docs.enabled);
        assert_eq!(docs.vault_path, None, "a full overwrite, not a merge");
    }

    #[tokio::test]
    async fn upserting_an_mcp_server_with_an_existing_id_replaces_it() {
        let (_dir, store) = store().await;
        let server = McpServerConfig::stdio("Git", "uvx").args(["mcp-server-git"]);
        store.upsert_mcp_server(server.clone()).await.unwrap();

        let disabled = McpServerConfig {
            enabled: false,
            ..server.clone()
        };
        store.upsert_mcp_server(disabled.clone()).await.unwrap();

        assert_eq!(store.list_mcp_servers().await.unwrap(), [disabled]);
    }

    #[tokio::test]
    async fn removing_an_mcp_server_drops_it_from_the_list() {
        let (_dir, store) = store().await;
        let keep = McpServerConfig::stdio("Keep", "npx");
        let doomed = McpServerConfig::stdio("Doomed", "npx");
        store.upsert_mcp_server(keep.clone()).await.unwrap();
        store.upsert_mcp_server(doomed.clone()).await.unwrap();

        store.remove_mcp_server(doomed.id).await.unwrap();

        assert_eq!(store.list_mcp_servers().await.unwrap(), [keep]);
    }

    #[tokio::test]
    async fn mcp_servers_and_profiles_live_in_the_store_independently() {
        let (_dir, store) = store().await;
        let profile = Profile::new("Local", "openai", "m");
        let server = McpServerConfig::stdio("Git", "uvx");

        store.upsert(profile.clone()).await.unwrap();
        store.upsert_mcp_server(server.clone()).await.unwrap();
        store.remove_mcp_server(server.id).await.unwrap();

        // Removing the server must not disturb the profile still saved
        // alongside it in the same document.
        assert_eq!(store.list().await.unwrap(), [profile]);
        assert!(store.list_mcp_servers().await.unwrap().is_empty());
    }
}
