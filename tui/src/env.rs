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
}
