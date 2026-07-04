use crate::{AgentServer, AgentServerDelegate, load_proxy_env};
use acp_thread::AgentConnection;
use agent_client_protocol::schema::v1 as acp;
use anyhow::{Context as _, Result};
use collections::{HashMap, HashSet};
use fs::Fs;
use gpui::{App, AppContext as _, Entity, Task};
use language_model::{ApiKey, EnvVar};
use project::{
    Project,
    agent_server_store::{AgentId, AllAgentServersSettings},
};
use settings::{AgentConfigOptionValue, SettingsStore, update_settings_file};
use std::{rc::Rc, sync::Arc};
use ui::IconName;
use util::ResultExt as _;

pub const GEMINI_ID: &str = "gemini";
pub const CLAUDE_AGENT_ID: &str = "claude-acp";
pub const CODEX_ID: &str = "codex-acp";
pub const CURSOR_ID: &str = "cursor";

/// A generic agent server implementation for custom user-defined agents
pub struct CustomAgentServer {
    agent_id: AgentId,
}

impl CustomAgentServer {
    pub fn new(agent_id: AgentId) -> Self {
        Self { agent_id }
    }
}

impl AgentServer for CustomAgentServer {
    fn agent_id(&self) -> AgentId {
        self.agent_id.clone()
    }

    fn logo(&self) -> IconName {
        IconName::Terminal
    }

    fn default_mode(&self, cx: &App) -> Option<acp::SessionModeId> {
        let settings = cx.read_global(|settings: &SettingsStore, _| {
            settings
                .get::<AllAgentServersSettings>(None)
                .get(self.agent_id().0.as_ref())
                .cloned()
        });

        settings
            .as_ref()
            .and_then(|s| s.default_mode().map(acp::SessionModeId::new))
    }

    fn favorite_config_option_value_ids(
        &self,
        config_id: &acp::SessionConfigId,
        cx: &mut App,
    ) -> HashSet<acp::SessionConfigValueId> {
        let settings = cx.read_global(|settings: &SettingsStore, _| {
            settings
                .get::<AllAgentServersSettings>(None)
                .get(self.agent_id().0.as_ref())
                .cloned()
        });

        settings
            .as_ref()
            .and_then(|s| s.favorite_config_option_values(config_id.0.as_ref()))
            .map(|values| {
                values
                    .iter()
                    .cloned()
                    .map(acp::SessionConfigValueId::new)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn toggle_favorite_config_option_value(
        &self,
        config_id: acp::SessionConfigId,
        value_id: acp::SessionConfigValueId,
        should_be_favorite: bool,
        fs: Arc<dyn Fs>,
        cx: &App,
    ) {
        let agent_id = self.agent_id();
        let config_id = config_id.to_string();
        let value_id = value_id.to_string();

        update_settings_file(fs, cx, move |settings, _cx| {
            let settings = settings
                .agent_servers
                .get_or_insert_default()
                .entry(agent_id.0.to_string())
                .or_insert_with(default_settings_for_agent);

            match settings {
                settings::CustomAgentServerSettings::Custom {
                    favorite_config_option_values,
                    ..
                }
                | settings::CustomAgentServerSettings::Registry {
                    favorite_config_option_values,
                    ..
                } => {
                    let entry = favorite_config_option_values
                        .entry(config_id.clone())
                        .or_insert_with(Vec::new);

                    if should_be_favorite {
                        if !entry.iter().any(|v| v == &value_id) {
                            entry.push(value_id.clone());
                        }
                    } else {
                        entry.retain(|v| v != &value_id);
                        if entry.is_empty() {
                            favorite_config_option_values.remove(&config_id);
                        }
                    }
                }
            }
        });
    }

    fn set_default_mode(&self, mode_id: Option<acp::SessionModeId>, fs: Arc<dyn Fs>, cx: &mut App) {
        let agent_id = self.agent_id();
        update_settings_file(fs, cx, move |settings, _cx| {
            let settings = settings
                .agent_servers
                .get_or_insert_default()
                .entry(agent_id.0.to_string())
                .or_insert_with(default_settings_for_agent);

            match settings {
                settings::CustomAgentServerSettings::Custom { default_mode, .. }
                | settings::CustomAgentServerSettings::Registry { default_mode, .. } => {
                    *default_mode = mode_id.map(|m| m.to_string());
                }
            }
        });
    }

    fn default_config_option(&self, config_id: &str, cx: &App) -> Option<AgentConfigOptionValue> {
        let settings = cx.read_global(|settings: &SettingsStore, _| {
            settings
                .get::<AllAgentServersSettings>(None)
                .get(self.agent_id().as_ref())
                .cloned()
        });

        settings
            .as_ref()
            .and_then(|s| s.default_config_option(config_id).cloned())
    }

    fn set_default_config_option(
        &self,
        config_id: &str,
        value: Option<AgentConfigOptionValue>,
        fs: Arc<dyn Fs>,
        cx: &mut App,
    ) {
        let agent_id = self.agent_id();
        let config_id = config_id.to_string();
        update_settings_file(fs, cx, move |settings, _cx| {
            let settings = settings
                .agent_servers
                .get_or_insert_default()
                .entry(agent_id.0.to_string())
                .or_insert_with(default_settings_for_agent);

            match settings {
                settings::CustomAgentServerSettings::Custom {
                    default_config_options,
                    ..
                }
                | settings::CustomAgentServerSettings::Registry {
                    default_config_options,
                    ..
                } => {
                    if let Some(value) = value {
                        default_config_options.insert(config_id.clone(), value);
                    } else {
                        default_config_options.remove(&config_id);
                    }
                }
            }
        });
    }

    fn connect(
        &self,
        delegate: AgentServerDelegate,
        project: Entity<Project>,
        cx: &mut App,
    ) -> Task<Result<Rc<dyn AgentConnection>>> {
        let agent_id = self.agent_id();
        let default_mode = self.default_mode(cx);
        let registry_id = registry_id_for_agent(agent_id.clone(), cx);
        let is_registry_agent = registry_id.is_some();
        let default_config_options = cx.read_global(|settings: &SettingsStore, _| {
            settings
                .get::<AllAgentServersSettings>(None)
                .get(self.agent_id().as_ref())
                .map(|s| match s {
                    project::agent_server_store::CustomAgentServerSettings::Custom {
                        default_config_options,
                        ..
                    }
                    | project::agent_server_store::CustomAgentServerSettings::Registry {
                        default_config_options,
                        ..
                    } => default_config_options.clone(),
                })
                .unwrap_or_default()
        });

        if is_registry_agent {
            if let Some(registry_store) = project::AgentRegistryStore::try_global(cx) {
                registry_store.update(cx, |store, cx| store.refresh_if_stale(cx));
            }
        }

        let mut extra_env = load_proxy_env(cx);
        if delegate.store.read(cx).no_browser() {
            extra_env.insert("NO_BROWSER".to_owned(), "1".to_owned());
        }
        if let Some(registry_id) = registry_id.as_ref() {
            match registry_id.as_ref() {
                CLAUDE_AGENT_ID => {
                    extra_env.insert("ANTHROPIC_API_KEY".into(), "".into());
                }
                CODEX_ID => {
                    if let Ok(api_key) = std::env::var("CODEX_API_KEY") {
                        extra_env.insert("CODEX_API_KEY".into(), api_key);
                    }
                    if let Ok(api_key) = std::env::var("OPEN_AI_API_KEY") {
                        extra_env.insert("OPEN_AI_API_KEY".into(), api_key);
                    }
                }
                GEMINI_ID => {
                    extra_env.insert("SURFACE".to_owned(), "zed".to_owned());
                }
                _ => {}
            }
        }
        let store = delegate.store.downgrade();
        cx.spawn(async move |cx| {
            if registry_id
                .as_ref()
                .is_some_and(|id| id.as_ref() == GEMINI_ID)
            {
                if let Some(api_key) = cx.update(api_key_for_gemini_cli).await.ok() {
                    extra_env.insert("GEMINI_API_KEY".into(), api_key);
                }
            }
            // A keychain failure shouldn't prevent the agent from launching.
            if let Some(env_secrets) = cx
                .update(|cx| load_agent_env_secrets(&agent_id, cx))
                .await
                .log_err()
            {
                extra_env.extend(env_secrets);
            }
            let command = store
                .update(cx, |store, cx| {
                    let agent = store.get_external_agent(&agent_id).with_context(|| {
                        format!("Custom agent server `{}` is not registered", agent_id)
                    })?;
                    if let Some(new_version_available_tx) = delegate.new_version_available {
                        agent.set_new_version_available_tx(new_version_available_tx);
                    }
                    if let Some(loading_status_tx) = delegate.loading_status {
                        agent.set_loading_status_tx(loading_status_tx);
                    }
                    anyhow::Ok(agent.get_command(vec![], extra_env, &mut cx.to_async()))
                })??
                .await?;
            let connection = crate::acp::connect(
                agent_id,
                registry_id,
                project,
                command,
                store.clone(),
                default_mode,
                default_config_options,
                cx,
            )
            .await?;
            Ok(connection)
        })
    }

    fn into_any(self: Rc<Self>) -> Rc<dyn std::any::Any> {
        self
    }
}

fn api_key_for_gemini_cli(cx: &mut App) -> Task<Result<String>> {
    let env_var = EnvVar::new("GEMINI_API_KEY".into()).or(EnvVar::new("GOOGLE_AI_API_KEY".into()));
    if let Some(key) = env_var.value {
        return Task::ready(Ok(key));
    }
    let credentials_provider = zed_credentials_provider::global(cx);
    let api_url = google_ai::API_URL.to_string();
    cx.spawn(async move |cx| {
        Ok(
            ApiKey::load_from_system_keychain(&api_url, credentials_provider.as_ref(), cx)
                .await?
                .key()
                .to_string(),
        )
    })
}

const AGENT_ENV_SECRETS_USERNAME: &str = "env";

fn agent_env_secrets_key(agent_id: &AgentId) -> String {
    format!("zed_external_agent_env:{agent_id}")
}

fn serialize_agent_env_secrets(secrets: &HashMap<String, String>) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(secrets)?)
}

fn deserialize_agent_env_secrets(bytes: &[u8]) -> Result<HashMap<String, String>> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Loads the secret environment variables configured for an agent from the
/// system keychain. Returns an empty map when none are stored.
pub fn load_agent_env_secrets(
    agent_id: &AgentId,
    cx: &App,
) -> Task<Result<HashMap<String, String>>> {
    let credentials_provider = zed_credentials_provider::global(cx);
    let key = agent_env_secrets_key(agent_id);
    cx.spawn(async move |cx| {
        let Some((_, secrets)) = credentials_provider.read_credentials(&key, cx).await? else {
            return Ok(HashMap::default());
        };
        deserialize_agent_env_secrets(&secrets)
    })
}

/// Stores the secret environment variables for an agent in the system
/// keychain, replacing any previously stored set. An empty map deletes the
/// keychain entry.
pub fn save_agent_env_secrets(
    agent_id: &AgentId,
    secrets: HashMap<String, String>,
    cx: &App,
) -> Task<Result<()>> {
    let credentials_provider = zed_credentials_provider::global(cx);
    let key = agent_env_secrets_key(agent_id);
    cx.spawn(async move |cx| {
        if secrets.is_empty() {
            credentials_provider.delete_credentials(&key, cx).await
        } else {
            let secrets = serialize_agent_env_secrets(&secrets)?;
            credentials_provider
                .write_credentials(&key, AGENT_ENV_SECRETS_USERNAME, &secrets, cx)
                .await
        }
    })
}

#[cfg(test)]
fn is_registry_agent(agent_id: impl Into<AgentId>, cx: &App) -> bool {
    registry_id_for_agent(agent_id, cx).is_some()
}

fn registry_id_for_agent(agent_id: impl Into<AgentId>, cx: &App) -> Option<AgentId> {
    let agent_id = agent_id.into();
    let settings_registry_id = cx.read_global(|settings: &SettingsStore, _| {
        settings
            .get::<AllAgentServersSettings>(None)
            .get(agent_id.as_ref())
            .and_then(|s| {
                if let project::agent_server_store::CustomAgentServerSettings::Registry {
                    registry_id,
                    ..
                } = s
                {
                    Some(AgentId::new(
                        registry_id
                            .as_deref()
                            .unwrap_or_else(|| agent_id.as_ref())
                            .to_string(),
                    ))
                } else {
                    None
                }
            })
    });
    if settings_registry_id.is_some() {
        return settings_registry_id;
    }

    project::AgentRegistryStore::try_global(cx).and_then(|store| {
        store
            .read(cx)
            .agent(&agent_id)
            .is_some()
            .then_some(agent_id)
    })
}

fn default_settings_for_agent() -> settings::CustomAgentServerSettings {
    settings::CustomAgentServerSettings::Registry {
        registry_id: None,
        display_name: None,
        default_mode: None,
        env: Default::default(),
        default_config_options: Default::default(),
        favorite_config_option_values: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collections::HashMap;
    use gpui::TestAppContext;
    use project::agent_registry_store::{
        AgentRegistryStore, RegistryAgent, RegistryAgentMetadata, RegistryNpxAgent,
    };
    use settings::Settings as _;
    use ui::SharedString;

    fn init_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
    }

    fn init_registry_with_agents(cx: &mut TestAppContext, agent_ids: &[&str]) {
        let agents: Vec<RegistryAgent> = agent_ids
            .iter()
            .map(|id| {
                let id = SharedString::from(id.to_string());
                RegistryAgent::Npx(RegistryNpxAgent {
                    metadata: RegistryAgentMetadata {
                        id: AgentId::new(id.clone()),
                        name: id.clone(),
                        description: SharedString::from(""),
                        version: SharedString::from("1.0.0"),
                        repository: None,
                        website: None,
                        icon_path: None,
                    },
                    package: id,
                    args: Vec::new(),
                    env: HashMap::default(),
                })
            })
            .collect();
        cx.update(|cx| {
            AgentRegistryStore::init_test_global(cx, agents);
        });
    }

    fn set_agent_server_settings(
        cx: &mut TestAppContext,
        entries: Vec<(&str, settings::CustomAgentServerSettings)>,
    ) {
        cx.update(|cx| {
            AllAgentServersSettings::override_global(
                project::agent_server_store::AllAgentServersSettings(
                    entries
                        .into_iter()
                        .map(|(name, settings)| (name.to_string(), settings.into()))
                        .collect(),
                ),
                cx,
            );
        });
    }

    #[gpui::test]
    fn test_unknown_agent_is_not_registry(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| {
            assert!(!is_registry_agent("my-custom-agent", cx));
        });
    }

    #[gpui::test]
    fn test_agent_in_registry_store_is_registry(cx: &mut TestAppContext) {
        init_test(cx);
        init_registry_with_agents(cx, &["some-new-registry-agent"]);
        cx.update(|cx| {
            assert!(is_registry_agent("some-new-registry-agent", cx));
            assert!(!is_registry_agent("not-in-registry", cx));
        });
    }

    #[gpui::test]
    fn test_agent_with_registry_settings_type_is_registry(cx: &mut TestAppContext) {
        init_test(cx);
        set_agent_server_settings(
            cx,
            vec![(
                "agent-from-settings",
                settings::CustomAgentServerSettings::Registry {
                    registry_id: None,
                    display_name: None,
                    env: HashMap::default(),
                    default_mode: None,
                    default_config_options: HashMap::default(),
                    favorite_config_option_values: HashMap::default(),
                },
            )],
        );
        cx.update(|cx| {
            assert!(is_registry_agent("agent-from-settings", cx));
        });
    }

    #[gpui::test]
    fn test_registry_instance_uses_configured_registry_id(cx: &mut TestAppContext) {
        init_test(cx);
        set_agent_server_settings(
            cx,
            vec![(
                "agent-account",
                settings::CustomAgentServerSettings::Registry {
                    registry_id: Some("agent-from-registry".to_string()),
                    display_name: None,
                    env: HashMap::default(),
                    default_mode: None,
                    default_config_options: HashMap::default(),
                    favorite_config_option_values: HashMap::default(),
                },
            )],
        );
        cx.update(|cx| {
            assert!(is_registry_agent("agent-account", cx));
            assert_eq!(
                registry_id_for_agent("agent-account", cx),
                Some(AgentId::new("agent-from-registry"))
            );
        });
    }

    #[test]
    fn test_agent_env_secrets_round_trip() {
        let secrets = HashMap::from_iter([
            (
                "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
                "sk-ant-oat01-test".to_string(),
            ),
            ("ANTHROPIC_API_KEY".to_string(), "sk-ant-test".to_string()),
        ]);
        let bytes = serialize_agent_env_secrets(&secrets).expect("serialization should succeed");
        assert_eq!(
            deserialize_agent_env_secrets(&bytes).expect("deserialization should succeed"),
            secrets
        );

        let empty = HashMap::default();
        let bytes = serialize_agent_env_secrets(&empty).expect("serialization should succeed");
        assert_eq!(
            deserialize_agent_env_secrets(&bytes).expect("deserialization should succeed"),
            empty
        );
    }

    #[derive(Default)]
    struct FakeCredentialsProvider(std::sync::Mutex<HashMap<String, Vec<u8>>>);

    impl credentials_provider::CredentialsProvider for FakeCredentialsProvider {
        fn read_credentials<'a>(
            &'a self,
            url: &'a str,
            _cx: &'a gpui::AsyncApp,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<Option<(String, Vec<u8>)>>> + 'a>>
        {
            let value = self.0.lock().expect("lock poisoned").get(url).cloned();
            Box::pin(async move {
                Ok(value.map(|password| (AGENT_ENV_SECRETS_USERNAME.to_string(), password)))
            })
        }

        fn write_credentials<'a>(
            &'a self,
            url: &'a str,
            _username: &'a str,
            password: &'a [u8],
            _cx: &'a gpui::AsyncApp,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
            self.0
                .lock()
                .expect("lock poisoned")
                .insert(url.to_string(), password.to_vec());
            Box::pin(async move { Ok(()) })
        }

        fn delete_credentials<'a>(
            &'a self,
            url: &'a str,
            _cx: &'a gpui::AsyncApp,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
            self.0.lock().expect("lock poisoned").remove(url);
            Box::pin(async move { Ok(()) })
        }
    }

    #[gpui::test]
    async fn test_agent_env_secrets_keychain_storage(cx: &mut TestAppContext) {
        init_test(cx);
        cx.update(|cx| {
            cx.set_global(zed_credentials_provider::ZedCredentialsProvider(Arc::new(
                FakeCredentialsProvider::default(),
            )))
        });

        let work = AgentId::new("claude-work");
        let personal = AgentId::new("claude-personal");
        let secrets = HashMap::from_iter([(
            "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
            "sk-ant-oat01-test".to_string(),
        )]);

        cx.update(|cx| save_agent_env_secrets(&work, secrets.clone(), cx))
            .await
            .expect("saving secrets should succeed");

        let loaded = cx
            .update(|cx| load_agent_env_secrets(&work, cx))
            .await
            .expect("loading secrets should succeed");
        assert_eq!(loaded, secrets);

        let other_agent = cx
            .update(|cx| load_agent_env_secrets(&personal, cx))
            .await
            .expect("loading secrets should succeed");
        assert!(other_agent.is_empty());

        cx.update(|cx| save_agent_env_secrets(&work, HashMap::default(), cx))
            .await
            .expect("deleting secrets should succeed");
        let loaded = cx
            .update(|cx| load_agent_env_secrets(&work, cx))
            .await
            .expect("loading secrets should succeed");
        assert!(loaded.is_empty());
    }
}
