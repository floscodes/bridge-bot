use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};
mod interfaces;
pub use interfaces::ReticulumInterface;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, deserialize_with = "deserialize_services")]
    pub services: HashMap<String, Service>,
    #[serde(default)]
    pub bridges: Vec<Bridge>,
    #[serde(default)]
    pub retry: RetryConfig,
}

fn deserialize_services<'de, D>(
    deserializer: D,
) -> std::result::Result<HashMap<String, Service>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let entries = HashMap::<String, toml::Value>::deserialize(deserializer)?;
    entries
        .into_iter()
        .map(|(name, mut value)| {
            let table = value.as_table_mut().ok_or_else(|| {
                serde::de::Error::custom(format!("service {name:?} must be a table"))
            })?;
            if !table.contains_key("type") {
                match name.as_str() {
                    "telegram" | "lxmf" | "rrc" => {
                        table.insert("type".into(), toml::Value::String(name.clone()));
                    }
                    _ => {
                        return Err(serde::de::Error::custom(format!(
                            "service {name:?} requires an explicit type"
                        )));
                    }
                }
            }
            if !table.contains_key("identity_path")
                && matches!(
                    table.get("type").and_then(toml::Value::as_str),
                    Some("rrc" | "lxmf")
                )
                && !matches!(name.as_str(), "rrc" | "lxmf")
            {
                table.insert(
                    "identity_path".into(),
                    toml::Value::String(format!(".bridge-bot/identity-{}", hex::encode(&name))),
                );
            }
            let service = value.try_into().map_err(|error| {
                serde::de::Error::custom(format!("invalid service {name:?}: {error}"))
            })?;
            Ok((name, service))
        })
        .collect()
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub initial_delay_secs: u64,
    pub max_delay_secs: u64,
    pub max_age_secs: u64,
    pub attempt_timeout_secs: u64,
    pub queue_capacity: usize,
}
impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 6,
            initial_delay_secs: 2,
            max_delay_secs: 60,
            max_age_secs: 600,
            attempt_timeout_secs: 90,
            queue_capacity: 256,
        }
    }
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Service {
    Telegram {
        token: String,
    },
    Rrc {
        hub: String,
        #[serde(default = "default_rrc_identity")]
        identity_path: String,
        nickname: String,
        #[serde(default)]
        reticulum_config: Option<String>,
        #[serde(default = "default_interfaces")]
        interfaces: Vec<ReticulumInterface>,
    },
    Lxmf {
        #[serde(default = "default_lxmf_identity")]
        identity_path: String,
        #[serde(default = "default_lxmf_name")]
        display_name: String,
        #[serde(default)]
        reticulum_config: Option<String>,
        #[serde(default = "default_interfaces")]
        interfaces: Vec<ReticulumInterface>,
    },
}
fn default_rrc_identity() -> String {
    ".bridge-bot/rrc-identity".into()
}
fn default_lxmf_identity() -> String {
    ".bridge-bot/lxmf-identity".into()
}
fn default_interfaces() -> Vec<ReticulumInterface> {
    vec![ReticulumInterface::Auto {
        group_id: "reticulum".into(),
        discovery_port: 29716,
        data_port: 42671,
        devices: Vec::new(),
    }]
}
fn default_lxmf_name() -> String {
    "bridge-bot".into()
}
fn validate_network(interfaces: &[ReticulumInterface], directory: Option<&str>) -> Result<()> {
    ensure!(interfaces.len() <= 64, "at most 64 interfaces per service");
    if let Some(directory) = directory {
        ensure!(!directory.trim().is_empty(), "reticulum_config is empty");
    }
    for interface in interfaces {
        interface.validate()?;
    }
    Ok(())
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub service: String,
    #[serde(default)]
    pub chat: String,
    #[serde(default)]
    pub message_thread_id: Option<i32>,
    #[serde(default, deserialize_with = "deserialize_room")]
    pub room: Option<String>,
    #[serde(default)]
    pub lxmf_group: bool,
}
fn deserialize_room<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .map(|room| room.trim_start_matches('#').to_owned()))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bridge {
    pub from: Endpoint,
    pub to: Endpoint,
    #[serde(default)]
    pub bidirectional: bool,
}
impl Endpoint {
    fn matches_source(&self, source: &Self) -> bool {
        self.service == source.service
            && self.chat == source.chat
            && self.room == source.room
            && self.message_thread_id == source.message_thread_id
    }
    pub fn address(&self) -> &str {
        self.room.as_deref().unwrap_or(&self.chat)
    }
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self =
            toml::from_str(&std::fs::read_to_string(path).context("reading configuration")?)
                .context("invalid TOML configuration")?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=100).contains(&self.retry.max_attempts),
            "retry.max_attempts must be 1–100"
        );
        ensure!(
            self.retry.initial_delay_secs > 0
                && self.retry.initial_delay_secs <= self.retry.max_delay_secs
                && self.retry.max_delay_secs <= 3600,
            "retry delays must be positive, initial <= max <= 3600 seconds"
        );
        ensure!(
            (1..=86400).contains(&self.retry.max_age_secs),
            "retry.max_age_secs must be 1–86400"
        );
        ensure!(
            (1..=3600).contains(&self.retry.attempt_timeout_secs),
            "retry.attempt_timeout_secs must be 1–3600"
        );
        ensure!(
            (1..=4096).contains(&self.retry.queue_capacity),
            "retry.queue_capacity must be 1–4096"
        );
        for (name, service) in &self.services {
            ensure!(!name.trim().is_empty(), "empty service name");
            match service {
                Service::Telegram { token } => {
                    ensure!(!token.trim().is_empty(), "Telegram token is empty");
                }
                Service::Rrc {
                    hub,
                    identity_path,
                    nickname,
                    interfaces,
                    reticulum_config,
                } => {
                    validate_hash(hub)?;
                    validate_network(interfaces, reticulum_config.as_deref())?;
                    ensure!(
                        !identity_path.trim().is_empty(),
                        "RRC identity_path is empty"
                    );
                    ensure!(
                        !nickname.trim().is_empty()
                            && nickname.len() <= 32
                            && !nickname.chars().any(char::is_control),
                        "RRC nickname must be 1–32 UTF-8 bytes without control characters"
                    );
                }
                Service::Lxmf {
                    identity_path,
                    display_name,
                    interfaces,
                    reticulum_config,
                } => {
                    ensure!(
                        !identity_path.trim().is_empty(),
                        "LXMF identity_path is empty"
                    );
                    ensure!(
                        !display_name.trim().is_empty()
                            && display_name.len() <= 128
                            && !display_name.chars().any(char::is_control),
                        "LXMF display_name must be 1–128 bytes without control characters"
                    );
                    validate_network(interfaces, reticulum_config.as_deref())?;
                }
            }
        }
        for bridge in self.active_bridges() {
            ensure!(bridge.from != bridge.to, "a bridge cannot route to itself");
            for endpoint in [&bridge.from, &bridge.to] {
                let service = self
                    .services
                    .get(&endpoint.service)
                    .context("bridge references unknown service")?;
                match service {
                    Service::Telegram { .. } => {
                        ensure!(
                            endpoint.room.is_none(),
                            "Telegram uses chat and message_thread_id; room is only for RRC"
                        );
                        let id = endpoint
                            .chat
                            .parse::<i64>()
                            .context("Telegram chat must be a numeric chat ID")?;
                        ensure!(id != 0, "Telegram chat ID cannot be zero");
                        ensure!(
                            endpoint.message_thread_id.is_none_or(|id| id > 0),
                            "Telegram message_thread_id must be positive"
                        );
                    }
                    Service::Rrc { .. } => {
                        ensure!(
                            endpoint.chat.is_empty() && endpoint.message_thread_id.is_none(),
                            "RRC uses room; chat and message_thread_id are not supported"
                        );
                        let room = endpoint
                            .room
                            .as_deref()
                            .context("RRC endpoint requires room")?;
                        ensure!(
                            !room.trim().is_empty()
                                && room.len() <= 64
                                && room == room.to_lowercase()
                                && !room.chars().any(char::is_whitespace)
                                && !room.chars().any(char::is_control),
                            "RRC room must be lowercase, 1–64 UTF-8 bytes, without whitespace or control characters"
                        );
                    }
                    Service::Lxmf { .. } => {
                        ensure!(endpoint.room.is_none(), "LXMF has no rooms");
                        ensure!(
                            endpoint.message_thread_id.is_none(),
                            "message_thread_id is only for Telegram"
                        );
                        validate_hash(&endpoint.chat)?;
                    }
                }
            }
        }
        Ok(())
    }
    pub fn routes_from<'a>(
        &'a self,
        source: &'a Endpoint,
    ) -> impl Iterator<Item = (&'a Endpoint, &'a Endpoint)> + 'a {
        self.active_bridges().filter_map(move |bridge| {
            if bridge.from.matches_source(source) {
                Some((&bridge.from, &bridge.to))
            } else if bridge.bidirectional && bridge.to.matches_source(source) {
                Some((&bridge.to, &bridge.from))
            } else {
                None
            }
        })
    }
    pub fn destinations(&self, source: &Endpoint) -> Vec<Endpoint> {
        let mut seen = HashSet::new();
        self.routes_from(source)
            .map(|(_, destination)| destination.clone())
            .filter(|endpoint| seen.insert(endpoint.clone()))
            .collect()
    }
    pub fn active_bridges(&self) -> impl Iterator<Item = &Bridge> {
        self.bridges.iter().filter(|bridge| {
            self.services.contains_key(&bridge.from.service)
                && self.services.contains_key(&bridge.to.service)
        })
    }
    pub fn has_inactive_route(&self, source: &Endpoint) -> bool {
        self.bridges.iter().any(|bridge| {
            (bridge.from.matches_source(source)
                || (bridge.bidirectional && bridge.to.matches_source(source)))
                && (!self.services.contains_key(&bridge.from.service)
                    || !self.services.contains_key(&bridge.to.service))
        })
    }
}
fn validate_hash(hash: &str) -> Result<()> {
    ensure!(
        hash.len() == 32
            && hash
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
        "LXMF destination must be 32 lowercase hexadecimal characters"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_paths_default_per_service_and_allow_overrides() {
        let input = r#"
            [services.rrc]
            hub = "00000000000000000000000000000003"
            nickname = "bridge"
            [services.lxmf]
            [services.second_lxmf]
            type = "lxmf"
            [services.third_lxmf]
            type = "lxmf"
            identity_path = "existing-identity"
        "#;
        let config: Config = toml::from_str(input).unwrap();
        config.validate().unwrap();
        assert!(
            matches!(&config.services["rrc"], Service::Rrc { identity_path, .. } if identity_path == ".bridge-bot/rrc-identity")
        );
        assert!(
            matches!(&config.services["lxmf"], Service::Lxmf { identity_path, .. } if identity_path == ".bridge-bot/lxmf-identity")
        );
        assert!(
            matches!(&config.services["second_lxmf"], Service::Lxmf { identity_path, .. } if identity_path == &format!(".bridge-bot/identity-{}", hex::encode("second_lxmf")))
        );
        assert!(
            matches!(&config.services["third_lxmf"], Service::Lxmf { identity_path, .. } if identity_path == "existing-identity")
        );
    }
    #[test]
    fn independent_service_interfaces_and_legacy_lxmf_migration() {
        let input = r#"
            [services.rrc]
            hub = "00000000000000000000000000000003"
            identity_path = "rrc-identity"
            nickname = "bridge"
            [[services.rrc.interfaces]]
            type = "tcp_client"
            host = "rrc.example.org"
            port = 4242
            [services.lxmf]
            identity_path = "lxmf-identity"
            [[services.lxmf.interfaces]]
            type = "auto"
            group_id = "other-network"
            discovery_port = 29717
            data_port = 42672
            devices = ["eth0"]
        "#;
        let config: Config = toml::from_str(input).unwrap();
        config.validate().unwrap();
        assert!(
            matches!(&config.services["rrc"], Service::Rrc { interfaces, .. } if matches!(&interfaces[0], ReticulumInterface::TcpClient { host, port: 4242 } if host == "rrc.example.org"))
        );
        assert!(
            matches!(&config.services["lxmf"], Service::Lxmf { interfaces, .. } if matches!(&interfaces[0], ReticulumInterface::Auto { data_port: 42672, .. }))
        );
        for invalid in [
            input.replace("port = 4242", "port = 0"),
            input.replace("port = 4242", "port = 65536"),
            input.replace("tcp_client", "unsupported"),
            input.replace("rrc.example.org", "bad\\n[interfaces]"),
            input.replace("data_port = 42672", "data_port = 29717"),
            input.replace(
                "identity_path = \"lxmf-identity\"",
                "rpc_endpoint = \"127.0.0.1:4243\"\nsource = \"00000000000000000000000000000001\"",
            ),
        ] {
            assert!(
                toml::from_str::<Config>(&invalid)
                    .and_then(|c| c.validate().map_err(serde::de::Error::custom))
                    .is_err()
            );
        }
    }
    #[test]
    fn separates_telegram_thread_ids_and_rrc_room_names() {
        let mut c: Config = toml::from_str(include_str!("../bridge-bot.toml.example")).unwrap();
        c.services.insert(
            "rrc".into(),
            Service::Rrc {
                hub: "00000000000000000000000000000003".into(),
                identity_path: "identity".into(),
                nickname: "bridge".into(),
                reticulum_config: None,
                interfaces: default_interfaces(),
            },
        );
        c.bridges = vec![Bridge {
            from: toml::from_str(
                "service = 'telegram'\nlxmf_group = false\nchat = '-1001234567890'\nmessage_thread_id = 2\n",
            )
            .unwrap(),
            to: toml::from_str("service = 'rrc'\nlxmf_group = false\nroom = '#lobby'\n").unwrap(),
            bidirectional: true,
        }];
        c.validate().unwrap();
        assert_eq!(c.bridges[0].to.address(), "lobby");
        c.bridges[0].from.room = Some("#lobby".into());
        assert!(c.validate().is_err());
        c.bridges[0].from.room = None;
        c.bridges[0].from.chat.clear();
        assert!(c.validate().is_err());
        c.bridges[0].from.chat = "-1001234567890".into();
        c.bridges[0].to.chat = "#lobby".into();
        assert!(c.validate().is_err());
        c.bridges[0].to.chat.clear();
        c.bridges[0].to.message_thread_id = Some(2);
        assert!(c.validate().is_err());
        c.bridges[0].to.message_thread_id = None;
        c.bridges[0].to.room = None;
        assert!(c.validate().is_err());
        assert!(
            toml::from_str::<Endpoint>(
                "service = 'telegram'\nlxmf_group = false\nchat = '-1001234567890'\nroom = 2\n"
            )
            .is_err()
        );
    }
    #[test]
    fn routes_explicit_rooms_in_both_directions_without_fallback() {
        let mut c: Config = toml::from_str(include_str!("../bridge-bot.toml.example")).unwrap();
        c.services.insert(
            "rrc".into(),
            Service::Rrc {
                hub: "00000000000000000000000000000003".into(),
                identity_path: "identity".into(),
                nickname: "bridge".into(),
                reticulum_config: None,
                interfaces: default_interfaces(),
            },
        );
        let topic = |message_thread_id| Endpoint {
            lxmf_group: false,
            service: "telegram".into(),
            chat: "-100123".into(),
            message_thread_id,
            room: None,
        };
        let rrc = |name: &str| Endpoint {
            lxmf_group: false,
            service: "rrc".into(),
            chat: String::new(),
            message_thread_id: None,
            room: Some(name.into()),
        };
        c.bridges = vec![
            Bridge {
                from: topic(Some(42)),
                to: rrc("#lobby"),
                bidirectional: true,
            },
            Bridge {
                from: topic(Some(57)),
                to: rrc("#news"),
                bidirectional: true,
            },
            Bridge {
                from: topic(None),
                to: rrc("#ordinary"),
                bidirectional: true,
            },
        ];
        c.validate().unwrap();
        assert_eq!(c.destinations(&topic(Some(42))), vec![rrc("#lobby")]);
        assert_eq!(c.destinations(&topic(Some(57))), vec![rrc("#news")]);
        assert_eq!(c.destinations(&rrc("#lobby")), vec![topic(Some(42))]);
        assert_eq!(c.destinations(&topic(None)), vec![rrc("#ordinary")]);
        assert!(c.destinations(&topic(Some(99))).is_empty());
        assert!(c.destinations(&topic(Some(1))).is_empty());
        c.bridges.push(Bridge {
            from: topic(Some(42)),
            to: rrc("#news"),
            bidirectional: false,
        });
        c.bridges.push(Bridge {
            from: topic(Some(42)),
            to: rrc("#lobby"),
            bidirectional: false,
        });
        assert_eq!(
            c.destinations(&topic(Some(42))),
            vec![rrc("#lobby"), rrc("#news")]
        );
        c.bridges[0].from.message_thread_id = Some(0);
        assert!(c.validate().is_err());
        c.bridges[0].from.message_thread_id = Some(42);
        c.bridges[0].to.message_thread_id = Some(42);
        assert!(c.validate().is_err());
    }
    #[test]
    fn infers_standard_types_and_requires_types_for_custom_names() {
        let input = r#"
            bridges = []
            [services.telegram]
            token = "TEST_TOKEN"
            [services.lxmf]
            identity_path = "lxmf-identity"
            [services.rrc]
            hub = "00000000000000000000000000000003"
            identity_path = "identity"
            nickname = "bridge"
        "#;
        let config: Config = toml::from_str(input).unwrap();
        assert!(matches!(
            config.services["telegram"],
            Service::Telegram { .. }
        ));
        assert!(matches!(config.services["lxmf"], Service::Lxmf { .. }));
        assert!(matches!(config.services["rrc"], Service::Rrc { .. }));
        let custom = input.replace("[services.telegram]", "[services.telegram_work]");
        let error = toml::from_str::<Config>(&custom).err().unwrap().to_string();
        assert!(error.contains("telegram_work") && error.contains("explicit type"));
        let explicit = custom.replace("token =", "type = \"telegram\"\ntoken =");
        let config: Config = toml::from_str(&explicit).unwrap();
        assert!(matches!(
            config.services["telegram_work"],
            Service::Telegram { .. }
        ));
        let overridden = explicit.replace("[services.telegram_work]", "[services.rrc_alt]");
        let config: Config = toml::from_str(&overridden).unwrap();
        assert!(matches!(
            config.services["rrc_alt"],
            Service::Telegram { .. }
        ));
        let override_standard =
            "bridges = []\n[services.rrc]\ntype = \"telegram\"\ntoken = \"TEST_TOKEN\"";
        let config: Config = toml::from_str(override_standard).unwrap();
        assert!(matches!(config.services["rrc"], Service::Telegram { .. }));
        for invalid in [
            "bridges = []\n[services.telegram]\ntype = \"unknown\"\ntoken = \"TEST_TOKEN\"",
            "bridges = []\n[services.telegram]\ntoken = \"TEST_TOKEN\"\ntypo = true",
            "bridges = []\n[services.telegram]\n",
        ] {
            assert!(toml::from_str::<Config>(invalid).is_err());
        }
    }
    #[test]
    fn routing_directions_and_duplicate_targets() {
        let mut c: Config = toml::from_str(include_str!("../bridge-bot.toml.example")).unwrap();
        c.validate().unwrap();
        let a = c.bridges[0].from.clone();
        let b = c.bridges[0].to.clone();
        assert_eq!(c.destinations(&a), vec![b.clone()]);
        assert_eq!(c.destinations(&b), vec![a.clone()]);
        c.bridges[0].bidirectional = false;
        assert!(c.destinations(&b).is_empty());
        c.bridges.push(Bridge {
            from: a.clone(),
            to: b.clone(),
            bidirectional: false,
        });
        assert_eq!(c.destinations(&a), vec![b]);
    }
    #[test]
    fn rrc_routes_and_validation() {
        let mut c: Config = toml::from_str(
            r##"
            [services.telegram]
            type = "telegram"
            token = "TOKEN"
            [services.rrc]
            type = "rrc"
            hub = "00000000000000000000000000000003"
            identity_path = ".bridge-bot/rrc-identity"
            nickname = "bridge-bot"
            [[bridges]]
            from = { service = "telegram", chat = "-1001234567890", lxmf_group = false }
            to = { service = "rrc", room = "#lobby", lxmf_group = false }
            bidirectional = true
        "##,
        )
        .unwrap();
        c.validate().unwrap();
        assert_eq!(c.bridges[0].to.room.as_deref(), Some("lobby"));
        assert_eq!(
            c.destinations(&c.bridges[0].to),
            vec![c.bridges[0].from.clone()]
        );
        for room in ["", "#Lobby", "#two rooms", "#bad\n"] {
            c.bridges[0].to.room = Some(room.into());
            assert!(c.validate().is_err(), "accepted room {room:?}");
        }
        c.bridges[0].to.room = Some("#lobby".into());
        if let Service::Rrc { hub, .. } = c.services.get_mut("rrc").unwrap() {
            *hub = "invalid".into();
        }
        assert!(c.validate().is_err());
    }
    #[test]
    fn strips_leading_room_hashes_and_routes_both_directions() {
        let mut c: Config = toml::from_str(include_str!("../bridge-bot.toml.example")).unwrap();
        c.services.insert(
            "rrc".into(),
            toml::from_str::<Service>(
                "type = 'rrc'\nhub = '00000000000000000000000000000003'\nnickname = 'bridge'\n",
            )
            .unwrap(),
        );
        let telegram = c.bridges[0].from.clone();
        for configured in ["general", "#general", "##general"] {
            let room: Endpoint = toml::from_str(&format!(
                "service = 'rrc'\nlxmf_group = false\nroom = '{configured}'\n"
            ))
            .unwrap();
            assert_eq!(room.room.as_deref(), Some("general"));
            c.bridges = vec![Bridge {
                from: telegram.clone(),
                to: room.clone(),
                bidirectional: true,
            }];
            c.validate().unwrap();
            assert_eq!(c.destinations(&telegram), vec![room.clone()]);
            assert_eq!(c.destinations(&room), vec![telegram.clone()]);
        }
        for configured in ["#", "###"] {
            c.bridges[0].to = toml::from_str(&format!(
                "service = 'rrc'\nlxmf_group = false\nroom = '{configured}'\n"
            ))
            .unwrap();
            assert!(c.validate().is_err());
        }
    }
    #[test]
    fn skips_unregistered_services_but_rejects_invalid_active_addresses() {
        let mut c: Config = toml::from_str(include_str!("../bridge-bot.toml.example")).unwrap();
        c.bridges[0].to.chat = "invalid".into();
        assert!(c.validate().is_err());
        c.bridges[0].to.service = "missing".into();
        c.validate().unwrap();
        assert_eq!(c.active_bridges().count(), 0);
        assert!(c.destinations(&c.bridges[0].from).is_empty());
        assert!(c.has_inactive_route(&c.bridges[0].from));
    }

    #[test]
    fn absent_services_and_bridges_are_valid() {
        for text in [
            "",
            "[services]\n",
            "[retry]\nmax_attempts = 6\n",
            "[[bridges]]\nfrom = { service = 'telegram', chat = '-100123', lxmf_group = false }\nto = { service = 'rrc', room = '#lobby', lxmf_group = false }\n",
        ] {
            let c: Config = toml::from_str(text).unwrap();
            c.validate().unwrap();
            assert!(c.services.is_empty());
            assert_eq!(c.active_bridges().count(), 0);
        }
        let c: Config = toml::from_str("[services.telegram]\ntoken = 'TEST_TOKEN'\n").unwrap();
        c.validate().unwrap();
        assert!(c.bridges.is_empty());
    }
    #[test]
    fn lxmf_group_uses_configured_source_in_both_directions() {
        let mut config: Config =
            toml::from_str(include_str!("../bridge-bot.toml.example")).unwrap();
        config.bridges[0].to.lxmf_group = true;
        let mut incoming = config.bridges[0].to.clone();
        incoming.lxmf_group = false;
        let routes: Vec<_> = config.routes_from(&incoming).collect();
        assert_eq!(routes.len(), 1);
        assert!(routes[0].0.lxmf_group);
        assert_eq!(routes[0].1, &config.bridges[0].from);
        let target = config.bridges[0].from.clone();
        config.bridges[0].from = config.bridges[0].to.clone();
        config.bridges[0].to = target;
        config.bridges[0].bidirectional = false;
        let routes: Vec<_> = config.routes_from(&incoming).collect();
        assert_eq!(routes.len(), 1);
        assert!(routes[0].0.lxmf_group);
    }
}
