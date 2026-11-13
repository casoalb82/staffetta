use anyhow::Result;
use ita_link::{config::Config, runner};
use std::path::PathBuf;

const USAGE: &str = "ita-link <config.toml>        run the service\n\
ita-link check <config.toml>  validate the configuration and exit";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, path] if cmd == "check" => {
            let cfg = Config::load(&PathBuf::from(path))?;
            println!("config ok: {} networks, service nick {}", cfg.networks.len(), cfg.service_nick);
            Ok(())
        }
        [path] if path != "-h" && path != "--help" => {
            let cfg = Config::load(&PathBuf::from(path))?;
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                let _ = tx.send(());
            });
            runner::run(cfg, rx).await
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}
