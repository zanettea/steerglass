use std::collections::BTreeMap;
use std::sync::OnceLock;

use ratatui::style::Color;
use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Env {
    Prod,
    Stage,
    Dev,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvChoice {
    Auto,
    Prod,
    Stage,
    Dev,
    Local,
    None,
}

impl EnvChoice {
    pub const ALL: [EnvChoice; 6] = [
        EnvChoice::Auto,
        EnvChoice::Prod,
        EnvChoice::Stage,
        EnvChoice::Dev,
        EnvChoice::Local,
        EnvChoice::None,
    ];

    pub fn from_setting(s: &str) -> Option<Self> {
        match s {
            "prod" => Some(EnvChoice::Prod),
            "stage" => Some(EnvChoice::Stage),
            "dev" => Some(EnvChoice::Dev),
            "local" => Some(EnvChoice::Local),
            "none" => Some(EnvChoice::None),
            "auto" => Some(EnvChoice::Auto),
            _ => None,
        }
    }

    pub fn as_setting(self) -> Option<&'static str> {
        match self {
            EnvChoice::Auto => None,
            EnvChoice::Prod => Some("prod"),
            EnvChoice::Stage => Some("stage"),
            EnvChoice::Dev => Some("dev"),
            EnvChoice::Local => Some("local"),
            EnvChoice::None => Some("none"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EnvChoice::Auto => "Auto (detect from context name)",
            EnvChoice::Prod => "PROD (Production)",
            EnvChoice::Stage => "STAGE (Staging)",
            EnvChoice::Dev => "DEV (Development)",
            EnvChoice::Local => "LOCAL (Local)",
            EnvChoice::None => "None (unclassified)",
        }
    }

    pub fn env(self) -> Option<Env> {
        match self {
            EnvChoice::Auto => None,
            EnvChoice::Prod => Some(Env::Prod),
            EnvChoice::Stage => Some(Env::Stage),
            EnvChoice::Dev => Some(Env::Dev),
            EnvChoice::Local => Some(Env::Local),
            EnvChoice::None => None,
        }
    }
}

pub fn resolve_env(choices: &BTreeMap<String, String>, ctx: &str) -> Option<Env> {
    match choices.get(ctx).and_then(|s| EnvChoice::from_setting(s)) {
        Some(EnvChoice::None) => None,
        Some(EnvChoice::Auto) | None => detect_env(ctx),
        Some(choice) => choice.env(),
    }
}

impl Env {
    pub fn tag(self) -> &'static str {
        match self {
            Env::Prod => "PROD",
            Env::Stage => "STAGE",
            Env::Dev => "DEV",
            Env::Local => "LOCAL",
        }
    }

    pub fn color(self) -> Color {
        match self {
            Env::Prod => Color::Red,
            Env::Stage => Color::Yellow,
            Env::Dev => Color::Blue,
            Env::Local => Color::Cyan,
        }
    }
}

pub fn detect_env(ctx: &str) -> Option<Env> {
    static LOCAL: OnceLock<Regex> = OnceLock::new();
    static STAGE: OnceLock<Regex> = OnceLock::new();
    static PROD: OnceLock<Regex> = OnceLock::new();
    static DEV: OnceLock<Regex> = OnceLock::new();

    let local = LOCAL.get_or_init(|| {
        Regex::new(r"(?i)^(minikube|docker-desktop|docker-for-desktop|rancher-desktop|orbstack|colima)$|^(kind|k3d)-|localhost|127\.0\.0\.1").unwrap()
    });
    let stage = STAGE.get_or_init(|| {
        Regex::new(r"(?i)(^|[-_.:/@])(stage|staging|stg|uat|qa|pre-?prod)([-_.:/@]|$)").unwrap()
    });
    let prod = PROD.get_or_init(|| {
        Regex::new(r"(?i)(^|[-_.:/@])(prod|prd|production|live)([-_.:/@]|$)").unwrap()
    });
    let dev = DEV.get_or_init(|| {
        Regex::new(r"(?i)(^|[-_.:/@])(dev|develop|development|test|testing|sandbox|sbx)([-_.:/@]|$)").unwrap()
    });

    if local.is_match(ctx) {
        Some(Env::Local)
    } else if stage.is_match(ctx) {
        // Before PROD, so "pre-prod" counts as staging.
        Some(Env::Stage)
    } else if prod.is_match(ctx) {
        Some(Env::Prod)
    } else if dev.is_match(ctx) {
        Some(Env::Dev)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_environments() {
        assert_eq!(detect_env("kind-local"), Some(Env::Local));
        assert_eq!(detect_env("k3d-dev"), Some(Env::Local));
        assert_eq!(detect_env("minikube"), Some(Env::Local));
        assert_eq!(detect_env("shop-pre-prod"), Some(Env::Stage));
        assert_eq!(detect_env("gke_acme_us-central1_prod"), Some(Env::Prod));
        assert_eq!(detect_env("api-staging"), Some(Env::Stage));
        assert_eq!(detect_env("team-dev"), Some(Env::Dev));
        assert_eq!(detect_env("my-cluster"), None);
    }

    #[test]
    fn resolves_overrides_and_falls_back_to_detect() {
        let mut choices = BTreeMap::new();
        assert_eq!(resolve_env(&choices, "my-cluster"), None);
        assert_eq!(resolve_env(&choices, "api-prod"), Some(Env::Prod));

        choices.insert("my-cluster".into(), "prod".into());
        choices.insert("api-prod".into(), "none".into());
        assert_eq!(resolve_env(&choices, "my-cluster"), Some(Env::Prod));
        assert_eq!(resolve_env(&choices, "api-prod"), None);

        choices.insert("api-prod".into(), "dev".into());
        assert_eq!(resolve_env(&choices, "api-prod"), Some(Env::Dev));
    }
}
