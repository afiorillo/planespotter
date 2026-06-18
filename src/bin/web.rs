//! Planespotter web frontend: a split-flap (Solari) board of nearby aircraft.
//!
//! Subscribes to the same engine broadcast channel as the CLI, keeps a live board of aircraft
//! currently inside the watched regions, and streams it to the browser over SSE. The page is
//! fully self-contained (HTML/CSS/JS inlined) — no external/CDN assets.

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::Html;
use axum::routing::get;
use axum::Router;
use clap::Parser;
use futures_util::{stream, StreamExt};
use serde::Serialize;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

use planespotter::config::Config;
use planespotter::engine::Engine;
use planespotter::model::{Airport, EventKind, FlightInfo, SpottingEvent};

#[derive(Parser, Debug)]
#[command(name = "planespotter-web", about = "Split-flap board of nearby aircraft")]
struct Args {
    /// Path to the TOML config file.
    #[arg(short, long, default_value = "planespotter.toml")]
    config: PathBuf,

    /// Address to bind the web server to.
    #[arg(short, long, default_value = "127.0.0.1:8080")]
    addr: String,
}

/// One row on the board. Field strings are display-ready; the browser handles layout/animation.
#[derive(Debug, Clone, Default, Serialize)]
struct Row {
    id: String,
    region: String,
    flight: String,
    ac: String,
    from: String,
    to: String,
    alt: String,
    status: String,
}

impl Row {
    fn blank(id: &str, region: &str) -> Self {
        Row {
            id: id.to_string(),
            region: region.to_string(),
            ..Default::default()
        }
    }

    /// Apply an engine event. Aircraft-derived fields refresh on every update; route/status only
    /// when the event carries enrichment (i.e. on `Entered`), so they persist across `Updated`s.
    fn apply(&mut self, ev: &SpottingEvent) {
        let ac = &ev.aircraft;
        self.flight = ac
            .callsign
            .clone()
            .unwrap_or_else(|| ac.hex.to_uppercase());
        self.ac = ac.type_code.clone().unwrap_or_default();
        self.alt = ac
            .altitude_ft
            .map(|a| format!("{a:.0}"))
            .unwrap_or_default();
        if let Some(info) = &ev.flight_info {
            self.from = airport_code(&info.origin);
            self.to = airport_code(&info.destination);
            self.status = status_text(info);
        }
    }
}

fn airport_code(a: &Option<Airport>) -> String {
    a.as_ref()
        .and_then(|ap| ap.iata.clone().or_else(|| ap.icao.clone()))
        .unwrap_or_default()
}

fn status_text(info: &FlightInfo) -> String {
    match info.delay_minutes {
        None => String::new(),
        Some(0) => "ON TIME".to_string(),
        Some(d) if d > 0 => format!("+{d} MIN"),
        Some(d) => format!("{d} MIN"),
    }
}

type Board = HashMap<String, Row>;

#[derive(Clone)]
struct AppState {
    board: Arc<Mutex<Board>>,
    tx_board: broadcast::Sender<String>,
}

impl AppState {
    fn snapshot_json(&self) -> String {
        render_json(&self.board.lock().unwrap())
    }
}

/// Serialize the board as a JSON array of rows, sorted for stable ordering.
fn render_json(board: &Board) -> String {
    let mut rows: Vec<&Row> = board.values().collect();
    rows.sort_by(|a, b| a.flight.cmp(&b.flight).then_with(|| a.id.cmp(&b.id)));
    serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string())
}

fn apply_event(board: &mut Board, ev: &SpottingEvent) {
    let id = format!("{}\u{1}{}", ev.region, ev.aircraft.hex);
    match ev.kind {
        EventKind::Left => {
            board.remove(&id);
        }
        EventKind::Entered | EventKind::Updated => {
            board
                .entry(id.clone())
                .or_insert_with(|| Row::blank(&id, &ev.region))
                .apply(ev);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "planespotter=info,warn".into()),
        )
        .init();

    let args = Args::parse();
    let mut config = Config::load(&args.config)?;
    config.apply_env_secrets();

    let mut engine = Engine::from_config(&config)?;
    let mut engine_rx = engine.subscribe();

    let board: Arc<Mutex<Board>> = Arc::new(Mutex::new(HashMap::new()));
    let (tx_board, _) = broadcast::channel::<String>(32);
    let state = AppState {
        board: board.clone(),
        tx_board: tx_board.clone(),
    };

    // Consume engine events into the board state and broadcast the rendered JSON to clients.
    tokio::spawn(async move {
        loop {
            match engine_rx.recv().await {
                Ok(ev) => {
                    let json = {
                        let mut b = board.lock().unwrap();
                        apply_event(&mut b, &ev);
                        render_json(&b)
                    };
                    let _ = tx_board.send(json);
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // Run the polling engine in the background.
    tokio::spawn(async move {
        if let Err(e) = engine.run().await {
            tracing::error!(error = %e, "engine stopped");
        }
    });

    let app = Router::new()
        .route("/", get(index))
        .route("/events", get(events))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    tracing::info!("serving split-flap board on http://{}", args.addr);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

/// SSE stream: an immediate snapshot of the current board, then every subsequent update.
async fn events(
    State(state): State<AppState>,
) -> Sse<impl stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.tx_board.subscribe();
    let snapshot = state.snapshot_json();
    let head = stream::once(async move { Ok(Event::default().data(snapshot)) });
    let tail = BroadcastStream::new(rx)
        .filter_map(|msg| async move { msg.ok().map(|s| Ok(Event::default().data(s))) });
    Sse::new(head.chain(tail)).keep_alive(KeepAlive::default())
}
