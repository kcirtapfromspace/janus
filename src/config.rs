//! Settings: an optional ~/InterviewCoach/config.toml, overridden by `IC_*` environment variables.
//!
//! Every value is parsed into a typed field when ic starts, so a typo fails immediately with an
//! error naming the file key or variable, instead of surfacing later as a confusing failure.

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Find an executable on PATH.
pub fn which(cmd: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?).map(|dir| dir.join(cmd)).find(|p| p.is_file())
}

/// An LLM provider ic has a native adapter for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Anthropic,
    OpenAi,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Anthropic, Provider::OpenAi];

    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::OpenAi => "openai",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::Anthropic => "Anthropic",
            Provider::OpenAi => "OpenAI",
        }
    }

    /// The variable the LiteLLM proxy reads this provider's key from.
    pub fn key_var(self) -> &'static str {
        match self {
            Provider::Anthropic => "ANTHROPIC_API_KEY",
            Provider::OpenAi => "OPENAI_API_KEY",
        }
    }

    pub fn key_prefix(self) -> &'static str {
        match self {
            Provider::Anthropic => "sk-ant-",
            Provider::OpenAi => "sk-",
        }
    }

    pub fn keys_url(self) -> &'static str {
        match self {
            Provider::Anthropic => "https://platform.claude.com/settings/keys",
            Provider::OpenAi => "https://platform.openai.com/api-keys",
        }
    }
}

impl FromStr for Provider {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "anthropic" => Ok(Provider::Anthropic),
            "openai" => Ok(Provider::OpenAi),
            other => Err(format!("unknown provider {other:?} (expected anthropic or openai)")),
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A model on a specific provider, written `provider/model`, e.g. `openai/gpt-5.6`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: Provider,
    pub name: String,
}

impl FromStr for ModelRef {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let (provider, name) = s
            .split_once('/')
            .ok_or_else(|| format!("{s:?} needs a provider prefix, e.g. anthropic/claude-opus-5-5 or openai/gpt-5.6"))?;
        let name = name.trim();
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(format!("{s:?} has an invalid model name"));
        }
        Ok(ModelRef { provider: provider.trim().parse()?, name: name.to_string() })
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.name)
    }
}

impl Serialize for ModelRef {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ModelRef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

pub fn default_model() -> ModelRef {
    ModelRef { provider: Provider::Anthropic, name: "claude-opus-5-5".into() }
}

/// ~/InterviewCoach/config.toml. Every field is optional; unknown keys are an error.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    /// Model for analysis, e.g. "anthropic/claude-opus-5-5" or "openai/gpt-5.6".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelRef>,
    /// Interview language for Whisper ("en", "de", ...) or "auto".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// whisper.cpp model name, e.g. "large-v3-turbo" or "large-v3-turbo-q5_0".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub whisper_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_dir: Option<PathBuf>,
}

impl FileConfig {
    pub fn read(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("invalid {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(FileConfig::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        std::fs::create_dir_all(path.parent().context("config path has no parent")?)?;
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}

fn validate_language(value: &str) -> Result<Option<String>, String> {
    match value {
        "auto" => Ok(None),
        v if (2..=3).contains(&v.len()) && v.chars().all(|c| c.is_ascii_lowercase()) => Ok(Some(v.to_string())),
        v => Err(format!("{v:?} isn't a language code (use e.g. \"en\", or \"auto\")")),
    }
}

/// Read an `IC_*` variable and parse it, naming the variable in any error.
fn env_parse<T: FromStr>(var: &str) -> Result<Option<T>>
where
    T::Err: fmt::Display,
{
    match env::var(var) {
        Ok(v) => v.parse().map(Some).map_err(|e| anyhow!("{var}: {e}")),
        Err(_) => Ok(None),
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub data_dir: PathBuf,
    /// Downloaded models live in Caches: large, re-downloadable, and not worth backing up.
    pub models_dir: PathBuf,
    /// whisper.cpp model name, e.g. "large-v3-turbo" → ggml-large-v3-turbo.bin
    pub whisper_model: String,
    /// None lets Whisper detect the language; pinning it is more stable.
    pub language: Option<String>,
    /// Default model for analysis (`--model` overrides it per command).
    pub model: ModelRef,
}

fn home() -> PathBuf {
    dirs::home_dir().expect("no home directory")
}

impl Settings {
    pub fn load() -> Result<Self> {
        let data_dir = env::var_os("IC_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| home().join("InterviewCoach"));
        let file = FileConfig::read(&data_dir.join("config.toml"))?;
        let language = match env::var("IC_LANGUAGE") {
            Ok(v) => validate_language(&v).map_err(|e| anyhow!("IC_LANGUAGE: {e}"))?,
            Err(_) => validate_language(file.language.as_deref().unwrap_or("en")).map_err(|e| anyhow!("config.toml language: {e}"))?,
        };
        Ok(Settings {
            models_dir: env::var_os("IC_MODELS_DIR").map(PathBuf::from).or(file.models_dir).unwrap_or_else(|| {
                dirs::cache_dir().unwrap_or_else(|| home().join("Library/Caches")).join("InterviewCoach/models")
            }),
            whisper_model: env::var("IC_WHISPER_MODEL").ok().or(file.whisper_model).unwrap_or_else(|| "large-v3-turbo".into()),
            language,
            model: env_parse::<ModelRef>("IC_MODEL")?.or(file.model).unwrap_or_else(default_model),
            data_dir,
        })
    }

    pub fn config_path(&self) -> PathBuf {
        self.data_dir.join("config.toml")
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("coach.db")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.data_dir.join("sessions")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_refs_need_a_known_provider_prefix() {
        let m: ModelRef = "openai/gpt-5.6".parse().unwrap();
        assert_eq!((m.provider, m.name.as_str()), (Provider::OpenAi, "gpt-5.6"));
        assert_eq!(m.to_string(), "openai/gpt-5.6");
        assert!("claude-opus-5-5".parse::<ModelRef>().unwrap_err().contains("provider prefix"));
        assert!("mistral/large".parse::<ModelRef>().unwrap_err().contains("unknown provider"));
        assert!("anthropic/".parse::<ModelRef>().is_err());
    }

    #[test]
    fn config_file_is_typed_and_rejects_unknown_keys() {
        let ok: FileConfig = toml::from_str("model = \"openai/gpt-5.6\"\nlanguage = \"de\"\n").unwrap();
        assert_eq!(ok.model.unwrap().provider, Provider::OpenAi);
        let typo = toml::from_str::<FileConfig>("modle = \"openai/gpt-5.6\"\n").unwrap_err().to_string();
        assert!(typo.contains("modle"), "{typo}");
        let bad_model = toml::from_str::<FileConfig>("model = \"gpt-5.6\"\n").unwrap_err().to_string();
        assert!(bad_model.contains("provider prefix"), "{bad_model}");
    }

    #[test]
    fn config_roundtrips_through_toml() {
        let cfg = FileConfig { model: Some("openai/gpt-5.6".parse().unwrap()), ..Default::default() };
        let text = toml::to_string_pretty(&cfg).unwrap();
        assert_eq!(text.trim(), "model = \"openai/gpt-5.6\"");
        assert_eq!(toml::from_str::<FileConfig>(&text).unwrap(), cfg);
    }

    #[test]
    fn languages_are_validated() {
        assert_eq!(validate_language("auto"), Ok(None));
        assert_eq!(validate_language("en"), Ok(Some("en".into())));
        assert!(validate_language("English").is_err());
    }
}
