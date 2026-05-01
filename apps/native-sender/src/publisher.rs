use crate::api::TokenResponse;

#[derive(Debug, Clone, Copy)]
pub enum PublisherState {
    Starting,
    Running,
    Stopped,
    Error,
}

impl PublisherState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Error => "error",
        }
    }
}

pub async fn publish_bootstrap(platform_name: &str, token: &TokenResponse, dry_run: bool) {
    println!(
        "publisher config: platform={} livekit_url={} token_size={}",
        platform_name,
        token.url,
        token.token.len()
    );

    if dry_run {
        println!(
            "[dry-run] platform={} livekit_url={} token_prefix={}...",
            platform_name,
            token.url,
            &token.token[..std::cmp::min(token.token.len(), 10)]
        );
        return;
    }

    println!(
        "publisher bootstrap ready for platform={} (LiveKit room join integration next)",
        platform_name
    );
}

