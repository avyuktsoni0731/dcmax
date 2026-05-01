use crate::api::TokenResponse;

pub async fn publish_bootstrap(platform_name: &str, token: &TokenResponse, dry_run: bool) {
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
        "publisher bootstrap ready for platform={} (LiveKit integration next)",
        platform_name
    );
}

