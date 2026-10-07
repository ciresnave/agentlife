// SPDX-License-Identifier: MIT OR Apache-2.0
//! Configuration: every number is a default, never a ceiling (DESIGN-REVISION-2 §5, -3 §9).
//!
//! **Precedence: flag > environment variable > config file > built-in default.** The file is
//! `<home>/config.json`; an environment variable is `AGENTLIFE_<KEY>` in upper case; a flag is
//! whatever the CLI later passes to [`Layer::set`]. **An unknown key in the file refuses the
//! whole file**: a typo such as `free_ram_floor` must never read as "no limit".
//!
//! Nothing in here can *widen* authority: these keys narrow, delay or pace. Authority comes only
//! from a person's consent (DESIGN-REVISION-2 §7).

use serde::Deserialize;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Every config key, in documentation order. Also the list of keys read from the environment.
pub const KEYS: &[&str] = &[
    "free_ram_floor_gb",
    "max_running",
    "tabs_per_window",
    "retention_days",
    "batch_size",
    "batch_delay_secs",
    "liveness_timeout_secs",
    "network_wait_secs",
    "approval_default_duration",
    "pending_nag",
    "lazy_enabled",
    "lazy_after_idle_minutes",
    "idle_sweep_secs",
    "idle_stop_policy",
    "user_recent_minutes",
    "waiting_mark_ttl_hours",
    "pin_roles",
    "wake_poll_secs",
    "max_wakes_per_agent_per_hour",
    "max_wakes_per_minute",
    "synapse_addr",
    "synapse_account",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurationChoice {
    Hour,
    Day,
}

impl FromStr for DurationChoice {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "1h" => Ok(Self::Hour),
            "1d" => Ok(Self::Day),
            "forever" => Err("`forever` can never be the preselected duration".into()),
            other => Err(format!("expected 1h or 1d, got {other:?}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingNag {
    Quiet,
    OncePerUnlock,
    EveryMinutes(u32),
}

impl FromStr for PendingNag {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "quiet" => Ok(Self::Quiet),
            "once-per-unlock" => Ok(Self::OncePerUnlock),
            other => other
                .strip_prefix("every-")
                .and_then(|r| r.strip_suffix("-minutes"))
                .and_then(|n| n.parse::<u32>().ok())
                .filter(|n| *n >= 1)
                .map(Self::EveryMinutes)
                .ok_or_else(|| {
                    format!("expected quiet, once-per-unlock or every-N-minutes, got {other:?}")
                }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleStopPolicy {
    FreshElseAsk,
    FreshOnly,
    Ask,
    Skip,
}

impl FromStr for IdleStopPolicy {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "fresh-else-ask" => Ok(Self::FreshElseAsk),
            "fresh-only" => Ok(Self::FreshOnly),
            "ask" => Ok(Self::Ask),
            "skip" => Ok(Self::Skip),
            other => Err(format!(
                "expected fresh-else-ask, fresh-only, ask or skip, got {other:?}"
            )),
        }
    }
}

/// The resolved configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub free_ram_floor_gb: f64,
    pub max_running: Option<u32>,
    pub tabs_per_window: u32,
    pub retention_days: u32,
    pub batch_size: u32,
    pub batch_delay_secs: u64,
    pub liveness_timeout_secs: u64,
    pub network_wait_secs: u64,
    pub approval_default_duration: DurationChoice,
    pub pending_nag: PendingNag,
    pub lazy_enabled: bool,
    pub lazy_after_idle_minutes: u32,
    pub idle_sweep_secs: u64,
    pub idle_stop_policy: IdleStopPolicy,
    pub user_recent_minutes: u32,
    pub waiting_mark_ttl_hours: u32,
    pub pin_roles: Vec<String>,
    pub wake_poll_secs: u64,
    pub max_wakes_per_agent_per_hour: u32,
    pub max_wakes_per_minute: u32,
    pub synapse_addr: SocketAddr,
    pub synapse_account: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            free_ram_floor_gb: 8.0,
            max_running: None,
            tabs_per_window: 10,
            retention_days: 14,
            batch_size: 3,
            batch_delay_secs: 45,
            liveness_timeout_secs: 120,
            network_wait_secs: 120,
            approval_default_duration: DurationChoice::Day,
            pending_nag: PendingNag::OncePerUnlock,
            lazy_enabled: false,
            lazy_after_idle_minutes: 120,
            idle_sweep_secs: 60,
            idle_stop_policy: IdleStopPolicy::FreshElseAsk,
            user_recent_minutes: 30,
            waiting_mark_ttl_hours: 24,
            pin_roles: vec!["pm".to_string()],
            wake_poll_secs: 10,
            max_wakes_per_agent_per_hour: 6,
            max_wakes_per_minute: 10,
            synapse_addr: SocketAddr::from(([127, 0, 0, 1], 7920)),
            synapse_account: "ciresnave".to_string(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum ConfigError {
    /// A key that is not in [`KEYS`] (from a flag; a file reports it through `File`).
    UnknownKey(String),
    /// A value that does not parse for its key. `source` names where it came from.
    BadValue {
        key: String,
        value: String,
        why: String,
    },
    /// A parsed value that is out of range.
    Invalid { key: &'static str, why: String },
    /// The file exists but is unreadable, not valid JSON, has an unknown key, or has a wrong type.
    File { path: PathBuf, why: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::UnknownKey(k) => write!(f, "unknown config key {k:?}"),
            ConfigError::BadValue { key, value, why } => {
                write!(f, "bad value {value:?} for {key}: {why}")
            }
            ConfigError::Invalid { key, why } => write!(f, "invalid {key}: {why}"),
            ConfigError::File { path, why } => write!(f, "{}: {why}", path.display()),
        }
    }
}

impl std::error::Error for ConfigError {}

/// One source of configuration: every field optional.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub free_ram_floor_gb: Option<f64>,
    pub max_running: Option<u32>,
    pub tabs_per_window: Option<u32>,
    pub retention_days: Option<u32>,
    pub batch_size: Option<u32>,
    pub batch_delay_secs: Option<u64>,
    pub liveness_timeout_secs: Option<u64>,
    pub network_wait_secs: Option<u64>,
    pub approval_default_duration: Option<String>,
    pub pending_nag: Option<String>,
    pub lazy_enabled: Option<bool>,
    pub lazy_after_idle_minutes: Option<u32>,
    pub idle_sweep_secs: Option<u64>,
    pub idle_stop_policy: Option<String>,
    pub user_recent_minutes: Option<u32>,
    pub waiting_mark_ttl_hours: Option<u32>,
    pub pin_roles: Option<Vec<String>>,
    pub wake_poll_secs: Option<u64>,
    pub max_wakes_per_agent_per_hour: Option<u32>,
    pub max_wakes_per_minute: Option<u32>,
    pub synapse_addr: Option<String>,
    pub synapse_account: Option<String>,
}

fn parse<T: FromStr>(key: &str, value: &str) -> Result<T, ConfigError>
where
    T::Err: fmt::Display,
{
    value
        .trim()
        .parse::<T>()
        .map_err(|e| ConfigError::BadValue {
            key: key.to_string(),
            value: value.to_string(),
            why: e.to_string(),
        })
}

impl Layer {
    /// Sets one key from its text form (the form a flag or an environment variable has).
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), ConfigError> {
        match key {
            "free_ram_floor_gb" => self.free_ram_floor_gb = Some(parse(key, value)?),
            "max_running" => self.max_running = Some(parse(key, value)?),
            "tabs_per_window" => self.tabs_per_window = Some(parse(key, value)?),
            "retention_days" => self.retention_days = Some(parse(key, value)?),
            "batch_size" => self.batch_size = Some(parse(key, value)?),
            "batch_delay_secs" => self.batch_delay_secs = Some(parse(key, value)?),
            "liveness_timeout_secs" => self.liveness_timeout_secs = Some(parse(key, value)?),
            "network_wait_secs" => self.network_wait_secs = Some(parse(key, value)?),
            "approval_default_duration" => {
                self.approval_default_duration = Some(value.trim().to_string())
            }
            "pending_nag" => self.pending_nag = Some(value.trim().to_string()),
            "lazy_enabled" => self.lazy_enabled = Some(parse(key, value)?),
            "lazy_after_idle_minutes" => self.lazy_after_idle_minutes = Some(parse(key, value)?),
            "idle_sweep_secs" => self.idle_sweep_secs = Some(parse(key, value)?),
            "idle_stop_policy" => self.idle_stop_policy = Some(value.trim().to_string()),
            "user_recent_minutes" => self.user_recent_minutes = Some(parse(key, value)?),
            "waiting_mark_ttl_hours" => self.waiting_mark_ttl_hours = Some(parse(key, value)?),
            "pin_roles" => {
                self.pin_roles = Some(
                    value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                )
            }
            "wake_poll_secs" => self.wake_poll_secs = Some(parse(key, value)?),
            "max_wakes_per_agent_per_hour" => {
                self.max_wakes_per_agent_per_hour = Some(parse(key, value)?)
            }
            "max_wakes_per_minute" => self.max_wakes_per_minute = Some(parse(key, value)?),
            "synapse_addr" => self.synapse_addr = Some(value.trim().to_string()),
            "synapse_account" => self.synapse_account = Some(value.trim().to_string()),
            other => return Err(ConfigError::UnknownKey(other.to_string())),
        }
        Ok(())
    }

    /// Reads `AGENTLIFE_<KEY>` for every known key through `get`. An unset variable is skipped;
    /// a set one that does not parse is an error naming the variable. (An unrecognised
    /// `AGENTLIFE_*` variable cannot be told from an unrelated one and is ignored.)
    pub fn from_env_with(get: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut layer = Layer::default();
        for key in KEYS {
            let var = format!("AGENTLIFE_{}", key.to_ascii_uppercase());
            if let Some(v) = get(&var) {
                layer.set(key, &v).map_err(|e| match e {
                    ConfigError::BadValue { value, why, .. } => ConfigError::BadValue {
                        key: var.clone(),
                        value,
                        why,
                    },
                    other => other,
                })?;
            }
        }
        Ok(layer)
    }

    pub fn from_process_env() -> Result<Self, ConfigError> {
        Self::from_env_with(&|name| std::env::var(name).ok())
    }

    /// Parses a config file's text. Unknown keys and wrong types refuse the whole file.
    pub fn from_json(path: &Path, text: &str) -> Result<Self, ConfigError> {
        serde_json::from_str(text).map_err(|e| ConfigError::File {
            path: path.to_path_buf(),
            why: e.to_string(),
        })
    }

    /// Loads `path`. A missing file is an empty layer (no file means all defaults); any other
    /// problem refuses.
    pub fn load_file(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_json(path, &text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Layer::default()),
            Err(e) => Err(ConfigError::File {
                path: path.to_path_buf(),
                why: e.to_string(),
            }),
        }
    }
}

fn pick<T>(flag: Option<T>, env: Option<T>, file: Option<T>) -> Option<T> {
    flag.or(env).or(file)
}

fn valid_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn at_least_one<T: PartialOrd + From<u8> + fmt::Display>(
    key: &'static str,
    v: T,
) -> Result<T, ConfigError> {
    if v >= T::from(1) {
        Ok(v)
    } else {
        Err(ConfigError::Invalid {
            key,
            why: format!("must be at least 1, got {v}"),
        })
    }
}

fn typed<T: FromStr>(key: &str, v: Option<String>) -> Result<Option<T>, ConfigError>
where
    T::Err: fmt::Display,
{
    v.map(|s| parse::<T>(key, &s)).transpose()
}

impl Config {
    /// Resolves the three layers over the defaults, then validates the result.
    pub fn resolve(flags: Layer, env: Layer, file: Layer) -> Result<Config, ConfigError> {
        let d = Config::default();
        macro_rules! field {
            ($f:ident) => {
                pick(flags.$f, env.$f, file.$f).unwrap_or(d.$f)
            };
        }
        let floor = field!(free_ram_floor_gb);
        if !floor.is_finite() || floor < 0.0 {
            return Err(ConfigError::Invalid {
                key: "free_ram_floor_gb",
                why: format!("must be a finite number >= 0, got {floor}"),
            });
        }
        let synapse_addr = match pick(
            flags.synapse_addr.clone(),
            env.synapse_addr.clone(),
            file.synapse_addr.clone(),
        ) {
            Some(s) => {
                let a: SocketAddr = parse("synapse_addr", &s)?;
                if !a.ip().is_loopback() {
                    return Err(ConfigError::Invalid {
                        key: "synapse_addr",
                        why: format!("{a} is not a loopback address; Synapse binds loopback only"),
                    });
                }
                a
            }
            None => d.synapse_addr,
        };
        let pin_roles = field!(pin_roles);
        if let Some(bad) = pin_roles.iter().find(|r| !valid_identifier(r)) {
            return Err(ConfigError::Invalid {
                key: "pin_roles",
                why: format!("{bad:?} is not a plain identifier"),
            });
        }
        let synapse_account = field!(synapse_account);
        if !valid_identifier(&synapse_account) {
            return Err(ConfigError::Invalid {
                key: "synapse_account",
                why: format!("{synapse_account:?} is not a plain identifier"),
            });
        }
        let max_running = match pick(flags.max_running, env.max_running, file.max_running) {
            Some(n) => Some(at_least_one("max_running", n)?),
            None => None,
        };
        Ok(Config {
            free_ram_floor_gb: floor,
            max_running,
            tabs_per_window: at_least_one("tabs_per_window", field!(tabs_per_window))?,
            retention_days: field!(retention_days),
            batch_size: at_least_one("batch_size", field!(batch_size))?,
            batch_delay_secs: field!(batch_delay_secs),
            liveness_timeout_secs: at_least_one(
                "liveness_timeout_secs",
                field!(liveness_timeout_secs),
            )?,
            network_wait_secs: field!(network_wait_secs),
            approval_default_duration: typed(
                "approval_default_duration",
                pick(
                    flags.approval_default_duration.clone(),
                    env.approval_default_duration.clone(),
                    file.approval_default_duration.clone(),
                ),
            )?
            .unwrap_or(d.approval_default_duration),
            pending_nag: typed(
                "pending_nag",
                pick(
                    flags.pending_nag.clone(),
                    env.pending_nag.clone(),
                    file.pending_nag.clone(),
                ),
            )?
            .unwrap_or(d.pending_nag),
            lazy_enabled: field!(lazy_enabled),
            lazy_after_idle_minutes: field!(lazy_after_idle_minutes),
            idle_sweep_secs: at_least_one("idle_sweep_secs", field!(idle_sweep_secs))?,
            idle_stop_policy: typed(
                "idle_stop_policy",
                pick(
                    flags.idle_stop_policy.clone(),
                    env.idle_stop_policy.clone(),
                    file.idle_stop_policy.clone(),
                ),
            )?
            .unwrap_or(d.idle_stop_policy),
            user_recent_minutes: field!(user_recent_minutes),
            waiting_mark_ttl_hours: field!(waiting_mark_ttl_hours),
            pin_roles,
            wake_poll_secs: at_least_one("wake_poll_secs", field!(wake_poll_secs))?,
            max_wakes_per_agent_per_hour: at_least_one(
                "max_wakes_per_agent_per_hour",
                field!(max_wakes_per_agent_per_hour),
            )?,
            max_wakes_per_minute: at_least_one(
                "max_wakes_per_minute",
                field!(max_wakes_per_minute),
            )?,
            synapse_addr,
            synapse_account,
        })
    }

    /// The whole resolution a command does: flags given, the process environment, and the file
    /// under `home`.
    pub fn load(home: &crate::home::Home, flags: Layer) -> Result<Config, ConfigError> {
        Config::resolve(
            flags,
            Layer::from_process_env()?,
            Layer::load_file(&home.config_file())?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(key: &str, value: &str) -> Layer {
        let mut l = Layer::default();
        l.set(key, value).unwrap();
        l
    }

    fn file(json: &str) -> Layer {
        Layer::from_json(Path::new("config.json"), json).unwrap()
    }

    #[test]
    fn no_sources_means_the_documented_defaults() {
        let c = Config::resolve(Layer::default(), Layer::default(), Layer::default()).unwrap();
        assert_eq!(c, Config::default());
        // The numbers CireSnave ruled on (archive, decision 121).
        assert_eq!(c.free_ram_floor_gb, 8.0);
        assert_eq!(c.tabs_per_window, 10);
        assert_eq!(c.retention_days, 14);
        assert_eq!(c.max_running, None, "no hard cap by default");
        assert!(!c.lazy_enabled, "lazy ships off");
        assert_eq!(c.lazy_after_idle_minutes, 120);
        assert_eq!(c.pin_roles, ["pm"]);
    }

    #[test]
    fn precedence_is_flag_over_env_over_file_over_default() {
        let f = file(r#"{"tabs_per_window": 3}"#);
        let e = layer("tabs_per_window", "5");
        let g = layer("tabs_per_window", "7");
        let tabs = |flags, env, file| Config::resolve(flags, env, file).unwrap().tabs_per_window;
        assert_eq!(
            tabs(Layer::default(), Layer::default(), Layer::default()),
            10
        );
        assert_eq!(tabs(Layer::default(), Layer::default(), f.clone()), 3);
        assert_eq!(tabs(Layer::default(), e.clone(), f.clone()), 5);
        assert_eq!(tabs(g.clone(), e.clone(), f.clone()), 7);
        assert_eq!(tabs(g, Layer::default(), Layer::default()), 7);
    }

    #[test]
    fn an_unknown_key_in_the_file_refuses_the_whole_file() {
        let r = Layer::from_json(Path::new("config.json"), r#"{"free_ram_floor": 4}"#);
        match r {
            Err(ConfigError::File { why, .. }) => {
                assert!(
                    why.contains("free_ram_floor"),
                    "message should name the key: {why}"
                )
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        // Control: the correctly spelled key is accepted.
        assert!(Layer::from_json(Path::new("c"), r#"{"free_ram_floor_gb": 4}"#).is_ok());
    }

    #[test]
    fn a_wrong_type_in_the_file_refuses_rather_than_defaulting() {
        for bad in [
            r#"{"tabs_per_window": "ten"}"#,
            r#"{"lazy_enabled": "yes"}"#,
            r#"{"free_ram_floor_gb": true}"#,
            r#"{"pin_roles": "pm"}"#,
            "[1,2]",
            "not json",
        ] {
            assert!(
                matches!(
                    Layer::from_json(Path::new("c"), bad),
                    Err(ConfigError::File { .. })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_missing_file_is_all_defaults_but_a_present_bad_one_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let missing = d.path().join("config.json");
        assert_eq!(Layer::load_file(&missing).unwrap(), Layer::default());
        std::fs::write(&missing, "{ nope").unwrap();
        assert!(matches!(
            Layer::load_file(&missing),
            Err(ConfigError::File { .. })
        ));
    }

    #[test]
    fn the_environment_layer_reads_agentlife_prefixed_upper_case_keys() {
        let env = std::collections::HashMap::from([
            ("AGENTLIFE_FREE_RAM_FLOOR_GB".to_string(), "16".to_string()),
            ("AGENTLIFE_PIN_ROLES".to_string(), "pm, synapse".to_string()),
            (
                "AGENTLIFE_HOME".to_string(),
                "X:/not-a-config-key".to_string(),
            ),
            ("OTHER".to_string(), "1".to_string()),
        ]);
        let l = Layer::from_env_with(&|n| env.get(n).cloned()).unwrap();
        assert_eq!(l.free_ram_floor_gb, Some(16.0));
        assert_eq!(
            l.pin_roles,
            Some(vec!["pm".to_string(), "synapse".to_string()])
        );
        // AGENTLIFE_HOME is the home override, not a config key, and must not leak into config.
        let c = Config::resolve(Layer::default(), l, Layer::default()).unwrap();
        assert_eq!(c.tabs_per_window, 10);
    }

    #[test]
    fn a_bad_environment_value_is_an_error_naming_the_variable() {
        let env = std::collections::HashMap::from([(
            "AGENTLIFE_TABS_PER_WINDOW".to_string(),
            "lots".to_string(),
        )]);
        match Layer::from_env_with(&|n| env.get(n).cloned()) {
            Err(ConfigError::BadValue { key, value, .. }) => {
                assert_eq!(key, "AGENTLIFE_TABS_PER_WINDOW");
                assert_eq!(value, "lots");
            }
            other => panic!("expected BadValue, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_flag_key_is_refused() {
        let mut l = Layer::default();
        assert_eq!(
            l.set("tabs", "3"),
            Err(ConfigError::UnknownKey("tabs".to_string()))
        );
    }

    #[test]
    fn the_file_and_the_text_form_agree_for_every_key() {
        // Both input paths must produce the same Layer, or "flag" and "file" would disagree.
        let json = r#"{
            "free_ram_floor_gb": 12.5, "max_running": 9, "tabs_per_window": 4,
            "retention_days": 30, "batch_size": 6, "batch_delay_secs": 20,
            "liveness_timeout_secs": 90, "network_wait_secs": 10,
            "approval_default_duration": "1h", "pending_nag": "every-15-minutes",
            "lazy_enabled": true, "lazy_after_idle_minutes": 45, "idle_sweep_secs": 30,
            "idle_stop_policy": "skip", "user_recent_minutes": 15,
            "waiting_mark_ttl_hours": 12, "pin_roles": ["pm","synapse"],
            "wake_poll_secs": 5, "max_wakes_per_agent_per_hour": 2,
            "max_wakes_per_minute": 4, "synapse_addr": "127.0.0.1:9000",
            "synapse_account": "someone"
        }"#;
        let from_file = file(json);
        let text: &[(&str, &str)] = &[
            ("free_ram_floor_gb", "12.5"),
            ("max_running", "9"),
            ("tabs_per_window", "4"),
            ("retention_days", "30"),
            ("batch_size", "6"),
            ("batch_delay_secs", "20"),
            ("liveness_timeout_secs", "90"),
            ("network_wait_secs", "10"),
            ("approval_default_duration", "1h"),
            ("pending_nag", "every-15-minutes"),
            ("lazy_enabled", "true"),
            ("lazy_after_idle_minutes", "45"),
            ("idle_sweep_secs", "30"),
            ("idle_stop_policy", "skip"),
            ("user_recent_minutes", "15"),
            ("waiting_mark_ttl_hours", "12"),
            ("pin_roles", "pm,synapse"),
            ("wake_poll_secs", "5"),
            ("max_wakes_per_agent_per_hour", "2"),
            ("max_wakes_per_minute", "4"),
            ("synapse_addr", "127.0.0.1:9000"),
            ("synapse_account", "someone"),
        ];
        assert_eq!(text.len(), KEYS.len(), "the table must cover every key");
        let mut from_text = Layer::default();
        for (k, v) in text {
            assert!(KEYS.contains(k), "{k} is not a key");
            from_text.set(k, v).unwrap();
        }
        assert_eq!(from_file, from_text);
        let c = Config::resolve(from_text, Layer::default(), Layer::default()).unwrap();
        assert_eq!(c.pending_nag, PendingNag::EveryMinutes(15));
        assert_eq!(c.approval_default_duration, DurationChoice::Hour);
        assert_eq!(c.idle_stop_policy, IdleStopPolicy::Skip);
        assert_eq!(c.max_running, Some(9));
    }

    #[test]
    fn out_of_range_values_are_refused() {
        for (k, v) in [
            ("tabs_per_window", "0"),
            ("batch_size", "0"),
            ("max_running", "0"),
            ("liveness_timeout_secs", "0"),
            ("idle_sweep_secs", "0"),
            ("wake_poll_secs", "0"),
            ("max_wakes_per_agent_per_hour", "0"),
            ("max_wakes_per_minute", "0"),
            ("free_ram_floor_gb", "-1"),
            ("free_ram_floor_gb", "NaN"),
            ("free_ram_floor_gb", "inf"),
        ] {
            let r = Config::resolve(layer(k, v), Layer::default(), Layer::default());
            assert!(
                matches!(r, Err(ConfigError::Invalid { .. })),
                "{k}={v}: {r:?}"
            );
        }
        // Zero is a legitimate value where it means something.
        for (k, v) in [
            ("free_ram_floor_gb", "0"),
            ("lazy_after_idle_minutes", "0"),
            ("retention_days", "0"),
            ("batch_delay_secs", "0"),
        ] {
            assert!(
                Config::resolve(layer(k, v), Layer::default(), Layer::default()).is_ok(),
                "{k}={v}"
            );
        }
    }

    #[test]
    fn forever_can_never_be_the_default_duration() {
        let r = Config::resolve(
            layer("approval_default_duration", "forever"),
            Layer::default(),
            Layer::default(),
        );
        assert!(matches!(r, Err(ConfigError::BadValue { .. })), "{r:?}");
        assert!(Config::resolve(
            layer("approval_default_duration", "1d"),
            Layer::default(),
            Layer::default()
        )
        .is_ok());
    }

    #[test]
    fn synapse_must_be_loopback() {
        for bad in [
            "0.0.0.0:7920",
            "192.168.1.5:7920",
            "example.com:7920",
            "nonsense",
        ] {
            let r = Config::resolve(
                layer("synapse_addr", bad),
                Layer::default(),
                Layer::default(),
            );
            assert!(r.is_err(), "{bad} must be refused");
        }
        for good in ["127.0.0.1:7920", "[::1]:7920"] {
            assert!(
                Config::resolve(
                    layer("synapse_addr", good),
                    Layer::default(),
                    Layer::default()
                )
                .is_ok(),
                "{good}"
            );
        }
    }

    #[test]
    fn pin_roles_and_account_must_be_plain_identifiers() {
        for bad in ["pm;calc", "a b", ""] {
            let l = Layer {
                pin_roles: Some(vec![bad.to_string()]),
                ..Layer::default()
            };
            assert!(
                Config::resolve(l, Layer::default(), Layer::default()).is_err(),
                "{bad:?}"
            );
        }
        let r = Config::resolve(
            layer("synapse_account", "a&b"),
            Layer::default(),
            Layer::default(),
        );
        assert!(r.is_err());
    }

    #[test]
    fn the_enum_keys_parse_every_documented_spelling_and_refuse_the_rest() {
        assert_eq!("quiet".parse::<PendingNag>(), Ok(PendingNag::Quiet));
        assert_eq!(
            "once-per-unlock".parse::<PendingNag>(),
            Ok(PendingNag::OncePerUnlock)
        );
        assert_eq!(
            "every-1-minutes".parse::<PendingNag>(),
            Ok(PendingNag::EveryMinutes(1))
        );
        assert!("every-0-minutes".parse::<PendingNag>().is_err());
        assert!("every--minutes".parse::<PendingNag>().is_err());
        assert!("often".parse::<PendingNag>().is_err());
        for (s, p) in [
            ("fresh-else-ask", IdleStopPolicy::FreshElseAsk),
            ("fresh-only", IdleStopPolicy::FreshOnly),
            ("ask", IdleStopPolicy::Ask),
            ("skip", IdleStopPolicy::Skip),
        ] {
            assert_eq!(s.parse::<IdleStopPolicy>(), Ok(p));
        }
        assert!("never".parse::<IdleStopPolicy>().is_err());
    }

    #[test]
    fn the_memory_floor_is_a_plain_number_so_a_bigger_machine_needs_no_recompile() {
        // DESIGN-REVISION-2 §5.1: the floor is relative to the machine; here it is just data.
        for gb in ["8", "64", "1024", "0.5"] {
            let c = Config::resolve(
                layer("free_ram_floor_gb", gb),
                Layer::default(),
                Layer::default(),
            )
            .unwrap();
            assert_eq!(c.free_ram_floor_gb, gb.parse::<f64>().unwrap());
        }
    }
}
