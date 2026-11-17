use std::net::IpAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use rocket::config::LogLevel;
use rocket::figment::Figment;
use rocket::figment::providers::Env;

use linear_webhook::config::Config;
use linear_webhook::discord;
use linear_webhook::server::{self, App};

/// Forward Linear webhooks to Discord.
///
/// Each `[name]` table in the config file is served at `POST /<name>`.
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    /// Path to the TOML config file.
    #[arg(short, long, env = "LINEAR_WEBHOOK_CONFIG")]
    config: PathBuf,

    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1", env = "LINEAR_WEBHOOK_ADDRESS")]
    address: IpAddr,

    /// Port to listen on.
    #[arg(long, default_value_t = 9999, env = "LINEAR_WEBHOOK_PORT")]
    port: u16,

    /// Accept deliveries without checking Linear-Signature or webhookTimestamp.
    /// Only for local testing: anyone who can reach the server can post to Discord.
    #[arg(long)]
    dangerously_ignore_secret: bool,

    /// Send Discord API requests to this base URL instead of https://discord.com
    /// (for testing against a fake Discord).
    #[arg(long, hide = true)]
    discord_proxy: Option<String>,
}

#[rocket::main]
async fn main() -> ExitCode {
    let args = Args::parse();

    let config = match Config::load(&args.config, args.dangerously_ignore_secret) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {}: {e}", args.config.display());
            return ExitCode::FAILURE;
        }
    };
    if args.dangerously_ignore_secret {
        eprintln!(
            "WARNING: --dangerously-ignore-secret is set; signatures and timestamps are NOT checked"
        );
    }
    let names: Vec<&str> = config.servers.keys().map(String::as_str).collect();
    eprintln!("serving: /{}", names.join(", /"));

    // Rocket's release profile only logs critical errors; default to normal
    // logging but still let ROCKET_* env vars override it. CLI flags win.
    let figment = Figment::from(rocket::Config::default())
        .merge(("log_level", LogLevel::Normal))
        .merge(("ident", false))
        .merge(Env::prefixed("ROCKET_").global())
        .merge(("address", args.address))
        .merge(("port", args.port));

    let app = App {
        config,
        http: discord::client(args.discord_proxy.as_deref()),
    };
    match server::rocket(figment, app).launch().await {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
