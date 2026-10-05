//! What a Mac needs before Janus works, as checks the app shows in its Setup window
//! (`ic setup status --json`), and the steps that fix them (`ic setup run <step> --events`).
//!
//! `status` is pure logic over a `Probe`, so every state a new Mac can be in is unit-tested
//! without touching Docker, the network, or the keychain. The app adds two checks of its own that
//! only it can run: microphone permission and the recording self-test.

use serde::Serialize;

use crate::config::{Provider, Settings};
use crate::progress::Progress;
use crate::proxy::{self, KeyTarget, LlmEndpoint};
use crate::tools::Tool;
use crate::{auth, diarize, transcribe};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Done.
    Ok,
    /// Needs you (or a button) to do something.
    Action,
    /// Waiting on an earlier check.
    Blocked,
    /// Not needed, but available.
    Optional,
}

/// A fixable step `ic setup run` performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Start Docker Desktop (or OrbStack) and wait for it.
    Docker,
    /// Set up or start the AI proxy (LiteLLM).
    Proxy,
    /// Download the speech and speaker-detection models.
    Models,
    /// Everything above, in order.
    All,
    /// Stop and start the AI proxy (keys and spend history are kept).
    #[serde(rename = "restart-proxy")]
    RestartProxy,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionKind {
    Run {
        step: Step,
    },
    OpenUrl {
        url: String,
    },
    SignIn,
    /// Sign out, then sign in again with another account.
    SwitchAccount,
    ChatGptSignIn {
        account: Option<String>,
        new_account: bool,
        enable_plan: bool,
    },
    ChatGptSignOut,
    Key {
        target: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Action {
    pub label: String,
    #[serde(flatten)]
    pub kind: ActionKind,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub status: Status,
    /// Analysis can't work until this is ok.
    pub required: bool,
    pub title: String,
    pub detail: String,
    pub action: Option<Action>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SetupStatus {
    /// Every required check is ok.
    pub ready: bool,
    /// Required checks still to do.
    pub remaining: usize,
    pub model: String,
    pub checks: Vec<Check>,
    pub openai: crate::openai_auth::Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignIn {
    Missing,
    Expired,
    Ok,
}

/// Everything `status` needs to know about this Mac.
pub trait Probe {
    /// Core calls use native APIs; Docker is only an optional scoring dependency.
    fn native(&self) -> bool {
        false
    }
    fn openai(&self) -> Result<crate::openai_auth::Status, String> {
        Ok(crate::openai_auth::Status::default())
    }
    /// Bundled tools that are missing (a broken install).
    fn missing_tools(&self) -> Vec<&'static str>;
    fn docker_installed(&self) -> bool;
    fn docker_running(&self) -> bool;
    /// The proxy's URL when one outside this Mac is configured (`IC_LLM_URL`).
    fn external_proxy(&self) -> Option<String>;
    /// The local proxy's files and ic's key exist (it may not be running).
    fn proxy_set_up(&self) -> bool;
    fn proxy_ready(&self) -> bool;
    fn sign_in(&self) -> SignIn;
    /// Who's signed in to Claude, e.g. "you@example.com (Your Organization)".
    fn account(&self) -> Option<String>;
    /// Bytes of the transcription models still to download (0 = all there). The speaker-detection
    /// models, only used for single-track imports, download with them but aren't required.
    fn models_missing(&self) -> u64;
    fn has_key(&self, target: KeyTarget) -> bool;
    /// An ANTHROPIC_API_KEY in the proxy, which breaks sign-in requests.
    fn stray_anthropic_key(&self) -> bool;
    fn analysis_provider(&self) -> Provider;
    fn model(&self) -> String;
}

fn action(label: &str, kind: ActionKind) -> Option<Action> {
    Some(Action {
        label: label.to_string(),
        kind,
    })
}

fn gb(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1e9)
    } else {
        format!("{} MB", bytes.div_ceil(1_000_000))
    }
}

pub fn status(p: &dyn Probe) -> SetupStatus {
    let mut checks = vec![];
    let missing = p.missing_tools();
    if !missing.is_empty() {
        checks.push(Check {
            id: "app",
            status: Status::Action,
            required: true,
            title: "Reinstall Janus".into(),
            detail: format!("Parts of the app are missing ({}). Download it again and replace the copy in Applications.",
                            missing.join(", ")),
            action: action("Download", ActionKind::OpenUrl {
                url: "https://github.com/kcirtapfromspace/interview-coach-releases/releases/latest".into(),
            }),
        });
    }

    if p.native() && p.external_proxy().is_none() {
        return native_status(p, checks);
    }
    let external = p.external_proxy();
    let proxy_ready = p.proxy_ready();
    if external.is_none() {
        let docker = if !p.docker_installed() {
            Check {
                id: "docker",
                status: Status::Action,
                required: true,
                title: "Install Docker Desktop".into(),
                detail: "Janus runs its AI proxy in Docker. Install Docker Desktop (free for personal use), \
                         open it once and accept its terms, then come back here."
                    .into(),
                action: action("Get Docker Desktop", ActionKind::OpenUrl {
                    url: "https://www.docker.com/products/docker-desktop/".into(),
                }),
            }
        } else if !p.docker_running() && !proxy_ready {
            Check {
                id: "docker",
                status: Status::Action,
                required: true,
                title: "Start Docker".into(),
                detail: "Docker is installed but not running. Janus starts it when it needs it; to have it \
                         ready, turn on \u{201c}Start Docker Desktop when you sign in\u{201d} in Docker's settings."
                    .into(),
                action: action("Start Docker", ActionKind::Run { step: Step::Docker }),
            }
        } else {
            Check {
                id: "docker",
                status: Status::Ok,
                required: true,
                title: "Docker is running".into(),
                detail: String::new(),
                action: None,
            }
        };
        let docker_ok = docker.status == Status::Ok;
        checks.push(docker);

        checks.push(if proxy_ready {
            Check { id: "proxy", status: Status::Ok, required: true, title: "AI proxy is running".into(),
                    detail: "LiteLLM, on this Mac only. Every AI request goes through it.".into(),
                    action: action("Restart", ActionKind::Run { step: Step::RestartProxy }) }
        } else if !docker_ok {
            Check { id: "proxy", status: Status::Blocked, required: true, title: "Set up the AI proxy".into(),
                    detail: "Needs Docker first.".into(), action: None }
        } else if p.proxy_set_up() {
            Check { id: "proxy", status: Status::Action, required: true, title: "Start the AI proxy".into(),
                    detail: "It's set up but not running.".into(),
                    action: action("Start", ActionKind::Run { step: Step::Proxy }) }
        } else {
            Check {
                id: "proxy",
                status: Status::Action,
                required: true,
                title: "Set up the AI proxy".into(),
                detail: "Downloads LiteLLM (about 2 GB, once) and runs it on this Mac. It brokers every AI request; \
                         nothing is exposed outside this Mac."
                    .into(),
                action: action("Set up", ActionKind::Run { step: Step::Proxy }),
            }
        });
        if p.stray_anthropic_key() {
            checks.push(Check {
                id: "stray_key",
                status: Status::Action,
                required: true,
                title: "Remove the Anthropic key from the AI proxy".into(),
                detail: "The proxy's settings (~/InterviewCoach/litellm/.env) contain ANTHROPIC_API_KEY. Anthropic \
                         rejects requests that carry both a key and your sign-in, so delete that line."
                    .into(),
                action: None,
            });
        }
    } else {
        let url = external.unwrap_or_default();
        checks.push(if proxy_ready {
            Check {
                id: "proxy",
                status: Status::Ok,
                required: true,
                title: "Using a shared AI proxy".into(),
                detail: url,
                action: None,
            }
        } else {
            Check {
                id: "proxy",
                status: Status::Action,
                required: true,
                title: "The shared AI proxy isn't answering".into(),
                detail: format!(
                    "{url} (set by IC_LLM_URL). Check it's running and that IC_LLM_KEY is right."
                ),
                action: None,
            }
        });
    }

    let claude_required = p.analysis_provider() == Provider::Anthropic;
    checks.push(match p.sign_in() {
        SignIn::Ok => Check {
            id: "claude",
            status: Status::Ok,
            required: claude_required,
            title: match p.account() {
                Some(account) => format!("Signed in to Claude as {account}"),
                None => "Signed in to Claude".into(),
            },
            detail: "Through your browser; no API key is stored. To use another account, sign in to it at claude.ai in \
                     your browser first, then switch."
                .into(),
            action: action("Switch Account…", ActionKind::SwitchAccount),
        },
        state => Check {
            id: "claude",
            status: if claude_required { Status::Action } else { Status::Optional },
            required: claude_required,
            title: if state == SignIn::Expired { "Sign in to Claude again".into() } else { "Sign in to Claude".into() },
            detail: if state == SignIn::Expired {
                "Your sign-in expired. Approve access in your browser again.".into()
            } else {
                "Approve access in your browser. No API key needed.".into()
            },
            action: action("Sign in", ActionKind::SignIn),
        },
    });

    let models = p.models_missing();
    checks.push(if models == 0 {
        Check {
            id: "models",
            status: Status::Ok,
            required: true,
            title: "Speech models downloaded".into(),
            detail: "Transcription runs on this Mac, so your audio never leaves it.".into(),
            action: None,
        }
    } else {
        Check {
            id: "models",
            status: Status::Action,
            required: true,
            title: "Download the speech models".into(),
            detail: format!(
                "About {}, once. Transcription runs on this Mac, so your audio never leaves it.",
                gb(models)
            ),
            action: action("Download", ActionKind::Run { step: Step::Models }),
        }
    });

    if p.external_proxy().is_none() {
        for target in KeyTarget::ALL {
            let required = target == KeyTarget::OpenAi && p.analysis_provider() == Provider::OpenAi;
            let (id, purpose) = match target {
                KeyTarget::OpenAi => ("openai_key", "Only needed to analyse with OpenAI models."),
                KeyTarget::TypeSafe => (
                    "typesafe_key",
                    "For Jev, which reads how the interviewer reacted, turn by turn.",
                ),
            };
            checks.push(if p.has_key(target) {
                Check {
                    id,
                    status: Status::Ok,
                    required,
                    title: format!("{} key added", target.label()),
                    detail: "Kept only in the AI proxy's settings on this Mac.".into(),
                    action: action(
                        "Replace",
                        ActionKind::Key {
                            target: target.as_str(),
                        },
                    ),
                }
            } else if !proxy_ready {
                Check {
                    id,
                    status: Status::Blocked,
                    required,
                    title: format!("Add your {} key", target.label()),
                    detail: format!("{purpose} Needs the AI proxy first."),
                    action: None,
                }
            } else {
                Check {
                    id,
                    status: if required {
                        Status::Action
                    } else {
                        Status::Optional
                    },
                    required,
                    title: format!(
                        "Add your {} key{}",
                        target.label(),
                        if required { "" } else { " (optional)" }
                    ),
                    detail: purpose.into(),
                    action: action(
                        "Add key",
                        ActionKind::Key {
                            target: target.as_str(),
                        },
                    ),
                }
            });
        }
    }

    let remaining = checks
        .iter()
        .filter(|c| c.required && c.status != Status::Ok)
        .count();
    SetupStatus {
        ready: remaining == 0,
        remaining,
        model: p.model(),
        checks,
        openai: p.openai().unwrap_or_default(),
    }
}

fn native_status(p: &dyn Probe, mut checks: Vec<Check>) -> SetupStatus {
    let claude_required = p.analysis_provider() == Provider::Anthropic;
    let claude = p.sign_in();
    checks.push(Check {
        id: "claude", required: claude_required,
        status: if claude == SignIn::Ok { Status::Ok } else if claude_required { Status::Action } else { Status::Optional },
        title: if claude == SignIn::Ok {
            p.account().map(|a| format!("Signed in to Claude as {a}")).unwrap_or_else(|| "Signed in to Claude".into())
        } else { "Sign in to Claude".into() },
        detail: "Approve access in your browser. Transcript text goes directly to Anthropic for coaching; recordings stay on this Mac.".into(),
        action: action(if claude == SignIn::Ok { "Switch Account…" } else { "Sign in" },
            if claude == SignIn::Ok { ActionKind::SwitchAccount } else { ActionKind::SignIn }),
    });
    let openai_result = p.openai();
    let openai_error = openai_result.as_ref().err().cloned();
    let openai = openai_result.unwrap_or_default();
    let required = p.analysis_provider() == Provider::OpenAi;
    let key_ready = openai.api_key || p.has_key(KeyTarget::OpenAi);
    let ready = if openai.active.is_some() && !openai.using_api_key {
        openai.plan_enabled
    } else {
        key_ready
    };
    let signed = openai.signed_in;
    checks.push(Check {
        id: "openai", required,
        status: if ready && openai_error.is_none() { Status::Ok } else if required { Status::Action } else { Status::Optional },
        title: if let Some(error) = openai_error { format!("OpenAI credentials need attention: {error}") }
            else if openai.using_api_key || (openai.active.is_none() && key_ready) { "OpenAI API billing selected".into() }
            else if signed { format!("Signed in with ChatGPT{}", openai.email.as_ref().map(|e| format!(" as {e}")).unwrap_or_default()) }
            else { "Sign in with ChatGPT".into() },
        detail: if signed && !openai.plan_enabled && !openai.using_api_key {
            "Sign-in succeeded, but ChatGPT plan usage is not enabled. Enable it below, or explicitly choose API billing by adding a key.".into()
        } else {
            "Eligible requests use your ChatGPT plan when authorized. Transcript text goes to OpenAI; recordings stay on this Mac. API billing is a separate choice.".into()
        },
        action: action(if signed && !openai.plan_enabled { "Enable plan usage" } else { "Continue with ChatGPT" },
            ActionKind::ChatGptSignIn { account: openai.active.clone(), new_account: false, enable_plan: signed && !openai.plan_enabled }),
    });
    let models = p.models_missing();
    checks.push(Check {
        id: "models",
        required: true,
        status: if models == 0 {
            Status::Ok
        } else {
            Status::Action
        },
        title: if models == 0 {
            "Speech models downloaded".into()
        } else {
            "Download the speech models".into()
        },
        detail: if models == 0 {
            "Transcription runs on this Mac. Audio is not uploaded.".into()
        } else {
            format!(
                "About {}, once. Transcription runs on this Mac. Audio is not uploaded.",
                gb(models)
            )
        },
        action: if models == 0 {
            None
        } else {
            action("Download", ActionKind::Run { step: Step::Models })
        },
    });
    checks.push(Check {
        id: "openai_key", required: false, status: if key_ready { Status::Ok } else { Status::Optional },
        title: "OpenAI API key (alternative billing)".into(),
        detail: "Adding a key explicitly selects API billing instead of ChatGPT plan usage. Kept in private local storage; Docker is not required.".into(),
        action: action(if key_ready { "Replace key" } else { "Add key" }, ActionKind::Key { target: "openai" }),
    });
    checks.push(Check {
        id: "typesafe_key", required: true,
        status: if p.has_key(KeyTarget::TypeSafe) { Status::Ok } else { Status::Action },
        title: if p.has_key(KeyTarget::TypeSafe) { "Jev evaluation configured".into() } else { "Connect Jev evaluation".into() },
        detail: "Jev is the core evaluator for answer quality and interviewer signals. Transcript excerpts go directly to TypeSafe. Its API key is stored privately on this Mac; Docker is not required.".into(),
        action: action(if p.has_key(KeyTarget::TypeSafe) { "Replace key" } else { "Add TypeSafe key" }, ActionKind::Key { target: "typesafe" }),
    });
    let remaining = checks
        .iter()
        .filter(|c| c.required && c.status != Status::Ok)
        .count();
    SetupStatus {
        ready: remaining == 0,
        remaining,
        model: p.model(),
        checks,
        openai,
    }
}

/// The real Mac.
pub struct System<'a> {
    pub settings: &'a Settings,
}

impl Probe for System<'_> {
    fn native(&self) -> bool {
        true
    }
    fn openai(&self) -> Result<crate::openai_auth::Status, String> {
        crate::openai_auth::status(self.settings).map_err(|e| e.to_string())
    }
    fn missing_tools(&self) -> Vec<&'static str> {
        [Tool::Ffmpeg, Tool::Ant]
            .into_iter()
            .filter(|t| *t != Tool::Ant || self.settings.model.provider == Provider::Anthropic)
            .filter(|t| t.find().is_none())
            .map(Tool::name)
            .collect()
    }

    fn docker_installed(&self) -> bool {
        proxy::docker_installed()
    }

    fn docker_running(&self) -> bool {
        proxy::docker_running()
    }

    fn external_proxy(&self) -> Option<String> {
        proxy::using_external_proxy().then(|| {
            LlmEndpoint::load(self.settings)
                .map(|e| e.base_url)
                .unwrap_or_default()
        })
    }

    fn proxy_set_up(&self) -> bool {
        proxy::dir(self.settings)
            .join("docker-compose.yml")
            .exists()
            && LlmEndpoint::load(self.settings).is_some()
    }

    fn proxy_ready(&self) -> bool {
        proxy::is_ready(self.settings)
    }

    fn account(&self) -> Option<String> {
        auth::account()
    }

    fn sign_in(&self) -> SignIn {
        match (
            auth::has_login(),
            auth::has_login() && auth::access_token().is_ok(),
        ) {
            (false, _) => SignIn::Missing,
            (true, false) => SignIn::Expired,
            (true, true) => SignIn::Ok,
        }
    }

    fn models_missing(&self) -> u64 {
        match transcribe::download_size(self.settings) {
            0 => 0,
            needed => needed + diarize::download_size(self.settings),
        }
    }

    fn has_key(&self, target: KeyTarget) -> bool {
        match target {
            KeyTarget::OpenAi => proxy::has_key(self.settings, target),
            KeyTarget::TypeSafe => crate::jev_auth::has_key(self.settings),
        }
    }

    fn stray_anthropic_key(&self) -> bool {
        proxy::has_stray_anthropic_key(self.settings)
    }

    fn analysis_provider(&self) -> Provider {
        self.settings.model.provider
    }

    fn model(&self) -> String {
        self.settings.model.to_string()
    }
}

/// Perform one setup step (the app shows its progress events).
pub fn run(step: Step, settings: &Settings, progress: &mut dyn Progress) -> anyhow::Result<()> {
    match step {
        Step::Docker => proxy::start_docker(progress),
        Step::Proxy => {
            let probe = System { settings };
            if probe.proxy_set_up() {
                proxy::ensure_running(settings, progress)
            } else {
                proxy::setup(settings, progress).map(|_| ())
            }
        }
        Step::Models => {
            transcribe::download(settings, progress)?;
            diarize::download(settings, progress)?;
            Ok(())
        }
        Step::All => run(Step::Models, settings, progress),
        Step::RestartProxy => {
            progress.stage("Stopping the AI proxy");
            proxy::stop(settings)?;
            progress.stage("Starting the AI proxy");
            proxy::ensure_running(settings, progress)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct Fake {
        missing_tools: Vec<&'static str>,
        docker_installed: bool,
        docker_running: bool,
        external: Option<String>,
        proxy_set_up: bool,
        proxy_ready: bool,
        sign_in: SignIn,
        models_missing: u64,
        keys: Vec<KeyTarget>,
        provider: Provider,
        native: bool,
        openai: crate::openai_auth::Status,
    }

    impl Fake {
        /// Everything done, with no optional keys.
        fn ready() -> Self {
            Fake {
                missing_tools: vec![],
                docker_installed: true,
                docker_running: true,
                external: None,
                proxy_set_up: true,
                proxy_ready: true,
                sign_in: SignIn::Ok,
                models_missing: 0,
                keys: vec![],
                provider: Provider::Anthropic,
                native: false,
                openai: crate::openai_auth::Status::default(),
            }
        }

        /// A Mac that has just downloaded the app.
        fn fresh() -> Self {
            Fake {
                docker_installed: false,
                docker_running: false,
                proxy_set_up: false,
                proxy_ready: false,
                sign_in: SignIn::Missing,
                models_missing: 1_700_000_000,
                ..Fake::ready()
            }
        }
    }

    impl Probe for Fake {
        fn native(&self) -> bool {
            self.native
        }
        fn openai(&self) -> Result<crate::openai_auth::Status, String> {
            Ok(self.openai.clone())
        }
        fn missing_tools(&self) -> Vec<&'static str> {
            self.missing_tools.clone()
        }
        fn docker_installed(&self) -> bool {
            self.docker_installed
        }
        fn docker_running(&self) -> bool {
            self.docker_running
        }
        fn external_proxy(&self) -> Option<String> {
            self.external.clone()
        }
        fn proxy_set_up(&self) -> bool {
            self.proxy_set_up
        }
        fn proxy_ready(&self) -> bool {
            self.proxy_ready
        }
        fn sign_in(&self) -> SignIn {
            self.sign_in
        }
        fn account(&self) -> Option<String> {
            (self.sign_in == SignIn::Ok).then(|| "you@example.com (Your Org)".into())
        }
        fn models_missing(&self) -> u64 {
            self.models_missing
        }
        fn has_key(&self, target: KeyTarget) -> bool {
            self.keys.contains(&target)
        }
        fn stray_anthropic_key(&self) -> bool {
            false
        }
        fn analysis_provider(&self) -> Provider {
            self.provider
        }
        fn model(&self) -> String {
            format!("{}/model", self.provider)
        }
    }

    fn summary(s: &SetupStatus) -> Vec<(&'static str, Status)> {
        s.checks.iter().map(|c| (c.id, c.status)).collect()
    }

    use Status::{Action as Act, Blocked, Ok as Done, Optional};

    #[test]
    fn native_fresh_mac_needs_provider_sign_in_speech_models_and_jev() {
        let s = status(&Fake {
            native: true,
            ..Fake::fresh()
        });
        assert_eq!(s.remaining, 3);
        assert!(s.checks.iter().all(|c| c.id != "docker" && c.id != "proxy"));
        assert!(
            s.checks
                .iter()
                .find(|c| c.id == "typesafe_key")
                .unwrap()
                .required
        );
        assert!(
            s.checks
                .iter()
                .find(|c| c.id == "openai")
                .unwrap()
                .action
                .as_ref()
                .unwrap()
                .label
                .contains("ChatGPT")
        );
    }

    #[test]
    fn native_coaching_with_jev_is_ready_without_docker_or_proxy() {
        let s = status(&Fake {
            native: true,
            docker_installed: false,
            docker_running: false,
            proxy_ready: false,
            proxy_set_up: false,
            keys: vec![KeyTarget::TypeSafe],
            ..Fake::ready()
        });
        assert!(s.ready);
        assert_eq!(s.remaining, 0);
    }

    #[test]
    fn chatgpt_plan_permission_is_required_and_api_billing_is_an_explicit_alternative() {
        let p = Fake {
            native: true,
            provider: Provider::OpenAi,
            keys: vec![KeyTarget::TypeSafe],
            openai: crate::openai_auth::Status {
                active: Some("oaiapp_one".into()),
                signed_in: true,
                api_key: true,
                ..Default::default()
            },
            ..Fake::ready()
        };
        let s = status(&p);
        assert!(
            !s.ready,
            "a stored API key must not bypass a missing ChatGPT grant"
        );
        let check = s.checks.iter().find(|c| c.id == "openai").unwrap();
        assert_eq!(check.action.as_ref().unwrap().label, "Enable plan usage");
        assert!(
            status(&Fake {
                openai: crate::openai_auth::Status {
                    plan_enabled: true,
                    ..p.openai.clone()
                },
                ..p.clone()
            })
            .ready
        );
        assert!(
            status(&Fake {
                openai: crate::openai_auth::Status {
                    using_api_key: true,
                    ..p.openai.clone()
                },
                ..p
            })
            .ready
        );
    }

    #[test]
    fn a_fresh_mac_needs_docker_first_and_can_sign_in_and_download_meanwhile() {
        let s = status(&Fake::fresh());
        assert_eq!(
            summary(&s),
            [
                ("docker", Act),
                ("proxy", Blocked),
                ("claude", Act),
                ("models", Act),
                ("openai_key", Blocked),
                ("typesafe_key", Blocked)
            ]
        );
        assert!(!s.ready);
        assert_eq!(s.remaining, 4);
        let docker = &s.checks[0];
        assert!(
            matches!(&docker.action.as_ref().unwrap().kind, ActionKind::OpenUrl { url } if url.contains("docker.com"))
        );
        assert!(s.checks[3].detail.contains("1.7 GB"));
    }

    #[test]
    fn docker_installed_but_stopped_offers_to_start_it() {
        let s = status(&Fake {
            docker_running: false,
            proxy_ready: false,
            ..Fake::ready()
        });
        assert_eq!(summary(&s)[..2], [("docker", Act), ("proxy", Blocked)]);
        assert_eq!(
            s.checks[0].action.as_ref().unwrap().kind,
            ActionKind::Run { step: Step::Docker }
        );
    }

    #[test]
    fn a_set_up_proxy_that_stopped_offers_start_not_setup() {
        let s = status(&Fake {
            proxy_ready: false,
            ..Fake::ready()
        });
        assert_eq!(s.checks[1].title, "Start the AI proxy");
        assert_eq!(
            s.checks[1].action.as_ref().unwrap().kind,
            ActionKind::Run { step: Step::Proxy }
        );
    }

    #[test]
    fn signed_out_or_expired_needs_a_sign_in() {
        let s = status(&Fake {
            sign_in: SignIn::Expired,
            ..Fake::ready()
        });
        let claude = s.checks.iter().find(|c| c.id == "claude").unwrap();
        assert_eq!(
            (claude.status, claude.title.as_str()),
            (Act, "Sign in to Claude again")
        );
        assert_eq!(claude.action.as_ref().unwrap().kind, ActionKind::SignIn);
        assert_eq!(s.remaining, 1);
    }

    #[test]
    fn everything_done_is_ready_and_keys_stay_optional() {
        let s = status(&Fake::ready());
        assert!(s.ready);
        assert_eq!(s.remaining, 0);
        assert_eq!(
            summary(&s)[4..],
            [("openai_key", Optional), ("typesafe_key", Optional)]
        );
        let with_jev = status(&Fake {
            keys: vec![KeyTarget::TypeSafe],
            ..Fake::ready()
        });
        assert_eq!(summary(&with_jev)[5], ("typesafe_key", Done));
    }

    #[test]
    fn analysing_with_openai_needs_its_key_but_not_claude() {
        let s = status(&Fake {
            provider: Provider::OpenAi,
            sign_in: SignIn::Missing,
            ..Fake::ready()
        });
        let get = |id| s.checks.iter().find(|c| c.id == id).unwrap();
        assert_eq!(
            (get("claude").status, get("claude").required),
            (Optional, false)
        );
        assert_eq!(
            (get("openai_key").status, get("openai_key").required),
            (Act, true)
        );
        assert_eq!(s.remaining, 1);
    }

    #[test]
    fn a_shared_proxy_skips_docker_and_keys() {
        let s = status(&Fake {
            external: Some("https://llm.example.com".into()),
            docker_installed: false,
            ..Fake::ready()
        });
        assert_eq!(
            summary(&s),
            [("proxy", Done), ("claude", Done), ("models", Done)]
        );
        assert!(s.ready);
    }

    #[test]
    fn a_broken_install_says_to_reinstall() {
        let s = status(&Fake {
            missing_tools: vec!["ffmpeg"],
            ..Fake::ready()
        });
        assert_eq!(summary(&s)[0], ("app", Act));
        assert!(s.checks[0].detail.contains("ffmpeg"));
    }

    /// Once everything works, the rows still let you change things: switch Claude accounts,
    /// restart the proxy, replace a key.
    #[test]
    fn finished_rows_can_still_be_changed() {
        let s = status(&Fake {
            keys: vec![KeyTarget::TypeSafe],
            ..Fake::ready()
        });
        let get = |id| s.checks.iter().find(|c| c.id == id).unwrap();
        assert_eq!(
            get("claude").title,
            "Signed in to Claude as you@example.com (Your Org)"
        );
        let json = serde_json::to_value(&s).unwrap();
        let action = |id: &str| {
            json["checks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == id)
                .unwrap()["action"]
                .clone()
        };
        assert_eq!(
            action("claude"),
            serde_json::json!({"label": "Switch Account…", "kind": "switch_account"})
        );
        assert_eq!(
            action("proxy"),
            serde_json::json!({"label": "Restart", "kind": "run", "step": "restart-proxy"})
        );
        assert_eq!(
            action("typesafe_key"),
            serde_json::json!({"label": "Replace", "kind": "key", "target": "typesafe"})
        );
        assert_eq!(get("docker").action, None);
    }

    /// The app passes a step's JSON name to `ic setup run`, so the two spellings must match.
    #[test]
    fn step_names_are_the_same_in_json_and_on_the_command_line() {
        use clap::ValueEnum;
        for step in Step::value_variants() {
            let cli = step.to_possible_value().unwrap().get_name().to_string();
            assert_eq!(
                serde_json::to_value(step).unwrap(),
                serde_json::Value::String(cli)
            );
        }
    }

    #[test]
    fn the_json_the_app_decodes() {
        let json = serde_json::to_value(status(&Fake::fresh())).unwrap();
        assert_eq!(
            json["checks"][0]["action"],
            serde_json::json!({
                "label": "Get Docker Desktop", "kind": "open_url", "url": "https://www.docker.com/products/docker-desktop/"
            })
        );
        assert_eq!(
            json["checks"][3]["action"],
            serde_json::json!({"label": "Download", "kind": "run", "step": "models"})
        );
        assert_eq!(
            json["checks"][2]["action"],
            serde_json::json!({"label": "Sign in", "kind": "sign_in"})
        );
        assert_eq!(json["checks"][1]["status"], "blocked");
    }
}
