use clap::Parser;

#[derive(Debug, Clone, Parser)]
#[command(name = "native-sender")]
#[command(about = "Native screen/audio sender bootstrap")]
pub struct CliArgs {
    #[arg(long)]
    pub room: Option<String>,
    #[arg(long)]
    pub identity: Option<String>,
    #[arg(long, default_value = "auto")]
    pub platform: String,
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub api_base_url: String,
    pub room_name: String,
    pub identity: String,
    pub client_type: String,
}

impl AppConfig {
    pub fn from_env(args: &CliArgs) -> Self {
        let api_base_url =
            std::env::var("API_BASE_URL").unwrap_or_else(|_| "http://localhost:4000".to_string());
        let room_name = args
            .room
            .clone()
            .or_else(|| std::env::var("ROOM_NAME").ok())
            .unwrap_or_else(|| "mycord-room".to_string());
        let identity = args
            .identity
            .clone()
            .or_else(|| std::env::var("IDENTITY").ok())
            .unwrap_or_else(|| "native-sender".to_string());
        let client_type =
            std::env::var("CLIENT_TYPE").unwrap_or_else(|_| "native_sender".to_string());

        Self {
            api_base_url,
            room_name,
            identity,
            client_type,
        }
    }
}

