//! Planespotter CLI: watch configured regions and print aircraft spottings as they happen.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use planespotter::config::Config;
use planespotter::engine::Engine;
use planespotter::model::{Airport, EventKind, FlightInfo, SpottingEvent};

#[derive(Parser, Debug)]
#[command(name = "planespotter", about = "Watch GPS regions and report aircraft inside them")]
struct Args {
    /// Path to the TOML config file.
    #[arg(short, long, default_value = "planespotter.toml")]
    config: PathBuf,

    /// Run a single scan and exit (instead of watching continuously).
    #[arg(long)]
    once: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Load a local .env (gitignored) so secrets like the AeroDataBox key stay out of config.
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "planespotter=info,warn".into()),
        )
        .init();

    let args = Args::parse();
    let mut config = Config::load(&args.config)?;
    // The AeroDataBox key is a secret: prefer the env var (from .env) over the config file.
    config.apply_env_secrets();

    let mut engine = Engine::from_config(&config)?;
    let mut rx = engine.subscribe();

    // Printer task owns the receiver and renders every event.
    let printer = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => println!("{}", format_event(&event)),
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("printer lagged, dropped {n} events");
                }
            }
        }
    });

    if args.once {
        engine.poll_once().await?;
        drop(engine); // closing the sender lets the printer drain and exit
        let _ = printer.await;
    } else {
        tracing::info!("watching {} region(s); Ctrl-C to stop", config.regions.len());
        tokio::select! {
            res = engine.run() => res?,
            _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
        }
    }

    Ok(())
}

/// Render a spotting event as a single human-readable line.
fn format_event(e: &SpottingEvent) -> String {
    let time = e.observed_at.format("%H:%M:%S");
    let tag = match e.kind {
        EventKind::Entered => "ENTER",
        EventKind::Updated => "  ···",
        EventKind::Left => "LEAVE",
    };

    let ident = e
        .aircraft
        .callsign
        .clone()
        .unwrap_or_else(|| e.aircraft.hex.to_uppercase());
    let typ = e.aircraft.type_code.as_deref().unwrap_or("?");

    let mut detail = String::new();
    if let Some(alt) = e.aircraft.altitude_ft {
        detail.push_str(&format!("{:.0}ft", alt));
    }
    if let Some(gs) = e.aircraft.ground_speed_kt {
        if !detail.is_empty() {
            detail.push_str(", ");
        }
        detail.push_str(&format!("{:.0}kt", gs));
    }

    let route = e.flight_info.as_ref().map(format_route).unwrap_or_default();

    let mut line = format!("[{time}] {tag} {region} | {ident} ({typ})", region = e.region);
    if !detail.is_empty() {
        line.push_str(&format!(" | {detail}"));
    }
    if !route.is_empty() {
        line.push_str(&format!(" | {route}"));
    }
    line
}

fn format_route(info: &FlightInfo) -> String {
    let code = |a: &Option<Airport>| {
        a.as_ref()
            .and_then(|ap| ap.iata.clone().or_else(|| ap.icao.clone()))
            .unwrap_or_else(|| "???".to_string())
    };
    let mut s = String::new();
    if info.origin.is_some() || info.destination.is_some() {
        s.push_str(&format!("{} → {}", code(&info.origin), code(&info.destination)));
    }
    if let Some(delay) = info.delay_minutes {
        let label = if delay > 0 {
            format!(" (+{delay} min)")
        } else if delay < 0 {
            format!(" ({delay} min early)")
        } else {
            " (on time)".to_string()
        };
        s.push_str(&label);
    }
    s
}
