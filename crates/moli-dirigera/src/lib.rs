//! IKEA Dirigera driver.
//!
//! Pairing is the hub's own ceremony (OAuth with PKCE): the driver asks
//! for a code, a human presses the action button under the hub, the hub
//! trades the code for a token. The token and the hub's certificate
//! fingerprint go to the encrypted secret store; from then on everything is
//! automatic: full load (`/v1/home`), then the hub's event websocket, with
//! commands sent as `PATCH` on devices.
//!
//! The driver never talks to a hub whose certificate it cannot pin, and
//! never gives up silently: a revoked token triggers a new pairing, a
//! changed certificate or a missing secret store becomes a `waiting` status
//! that says what a human must do.

mod hub;
mod model;
mod pkce;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, anyhow, bail};
use moli_core::DeviceId;
use moli_net::ws::WebSocket;
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::Value as Json;
use tokio::sync::mpsc;

pub use hub::{Authorize, Grant, Hub, PIN_MISMATCH};
pub use model::{Event, Index, Load, Skipped, Update};

const PAIRING_RETRY: Duration = Duration::from_secs(2);
/// A code lives 60 s; a hub that never says so still gets a new one.
const CODE_LIFETIME: Duration = Duration::from_secs(90);
/// The hub closes a quiet websocket: a ping event this often (lpgera's
/// rhythm), answered by a `pong` event.
const PING_EVERY: Duration = Duration::from_secs(30);
/// Nothing at all for this long, not even a pong: the connection is dead.
const SILENCE: Duration = Duration::from_secs(95);
/// A websocket that dies sooner than this was refused, not cut: the
/// supervisor's backoff takes over instead of reconnecting at once.
const SHORT_LIFE: Duration = Duration::from_secs(10);
const EVENT_QUEUE: usize = 64;
/// Below the hub's 5 s command timeout, so the answer is always ours.
const COMMAND_BUDGET: Duration = Duration::from_millis(4500);
const TOKEN: &str = "token";
const CERT: &str = "cert_sha256";

/// Driver configuration (`[driver.options]` in `moli.toml`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Hub IP or host name.
    pub host: String,
    /// Optional: the hub certificate's SHA-256 (hex), when it cannot be
    /// learned and stored at first contact (no secret store).
    #[serde(default)]
    pub cert_sha256: Option<String>,
}

#[derive(Debug)]
pub struct Dirigera {
    config: Config,
}

impl Dirigera {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Dirigera {
    fn kind(&self) -> &'static str {
        "dirigera"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// The hub refused our token.
#[derive(Debug)]
struct TokenRejected;

impl std::fmt::Display for TokenRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("dirigera token rejected")
    }
}

impl std::error::Error for TokenRejected {}

/// Shows `reason` on every surface and idles until shutdown.
async fn park(ctx: &DriverCtx, reason: String) -> anyhow::Result<()> {
    ctx.wait_for(reason);
    ctx.cancelled().await;
    Ok(())
}

fn secrets_problem(ctx: &DriverCtx) -> String {
    ctx.secrets_problem().map_or_else(
        || moli_i18n::tr!("pilotes.dirigera.coffre_indisponible"),
        str::to_owned,
    )
}

fn pin_changed(config: &Config, ctx: &DriverCtx) -> String {
    moli_i18n::tr!(
        "pilotes.dirigera.certificat_change",
        host = config.host,
        instance = ctx.instance(),
        key = CERT
    )
}

fn is_pin_mismatch(e: &anyhow::Error) -> bool {
    format!("{e:#}").contains(PIN_MISMATCH)
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let pin = match config.cert_sha256.clone().or_else(|| ctx.secret(CERT)) {
        Some(pin) => pin,
        None if !ctx.can_store_secrets() => {
            let reason = moli_i18n::tr!(
                "pilotes.dirigera.epinglage_impossible",
                problem = secrets_problem(ctx)
            );
            return park(ctx, reason).await;
        }
        // Pinned and stored before any code or token goes through.
        None => {
            let seen = Hub::first_contact(&config.host).await.context("dirigera")?;
            ctx.store_secret(CERT, &seen).await?;
            tracing::info!(instance = %ctx.instance(), "dirigera hub certificate pinned");
            seen
        }
    };

    let token = match ctx.secret(TOKEN) {
        Some(token) => token,
        None => match pair(config, ctx, pin.clone()).await? {
            Some(token) => token,
            None => return Ok(()), // shutting down while waiting
        },
    };

    let hub = Arc::new(Hub::new(&config.host, pin, Some(token))?);
    let mut index = match full_load(&hub, ctx).await {
        Ok(index) => index,
        Err(e) if e.is::<TokenRejected>() => {
            // Hub reset, or the token revoked: pair again.
            ctx.forget_secret(TOKEN).await?;
            return Err(e.context("pairing again"));
        }
        Err(e) if is_pin_mismatch(&e) => return park(ctx, pin_changed(config, ctx)).await,
        Err(e) => return Err(e),
    };
    ctx.ready();

    loop {
        let socket = match hub.events().await {
            Ok(socket) => socket,
            Err(e) if is_pin_mismatch(&e) => return park(ctx, pin_changed(config, ctx)).await,
            Err(e) => return Err(e.context("dirigera event websocket")),
        };
        let opened = Instant::now();
        let (tx, mut rx) = mpsc::channel(EVENT_QUEUE);
        let reader = tokio::spawn(read_events(socket, tx));
        tracing::debug!(instance = %ctx.instance(), "dirigera events open");
        loop {
            tokio::select! {
                message = rx.recv() => {
                    let Some(text) = message else { break };
                    if apply_message(ctx, &mut index, &text) {
                        tracing::info!(instance = %ctx.instance(), "dirigera devices changed, reloading");
                        index = full_load(&hub, ctx).await?;
                    }
                }
                command = ctx.next_command() => {
                    let Some(command) = command else {
                        reader.abort();
                        return Ok(());
                    };
                    spawn_command(&hub, &index, command);
                }
            }
        }
        reader.abort();
        if opened.elapsed() < SHORT_LIFE {
            bail!("the dirigera event websocket closed at once");
        }
        // The events of the cut are lost: start again from the truth.
        index = full_load(&hub, ctx).await?;
    }
}

/// Action-button pairing. Returns the token, or `None` on shutdown.
async fn pair(config: &Config, ctx: &mut DriverCtx, pin: String) -> anyhow::Result<Option<String>> {
    if !ctx.can_store_secrets() {
        let reason = moli_i18n::tr!(
            "pilotes.dirigera.jeton_non_enregistrable",
            problem = secrets_problem(ctx)
        );
        park(ctx, reason).await?;
        return Ok(None);
    }
    let hub = Hub::new(&config.host, pin, None)?;
    let name = format!("moli-os#{}", ctx.instance());
    ctx.wait_for(moli_i18n::tr!(
        "pilotes.dirigera.appuyer_bouton",
        host = config.host
    ));
    tracing::info!(instance = %ctx.instance(), "waiting for the dirigera action button");
    loop {
        let verifier = pkce::verifier()?;
        let code = match hub.authorize(&pkce::challenge(&verifier)).await {
            Ok(Authorize::Code(code)) => code,
            // One pairing at a time on the hub: wait for the other to end.
            Ok(Authorize::Busy) => {
                tracing::debug!(instance = %ctx.instance(), "another dirigera pairing request is ongoing");
                tokio::select! {
                    () = ctx.cancelled() => return Ok(None),
                    () = tokio::time::sleep(PAIRING_RETRY) => {}
                }
                continue;
            }
            Err(e) if is_pin_mismatch(&e) => {
                park(ctx, pin_changed(config, ctx)).await?;
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let asked = Instant::now();
        while asked.elapsed() < CODE_LIFETIME {
            tokio::select! {
                () = ctx.cancelled() => return Ok(None),
                () = tokio::time::sleep(PAIRING_RETRY) => {}
            }
            match hub.token(&code, &verifier, &name).await {
                Ok(Grant::Token(token)) => {
                    // The pin is stored already: a token never exists without it.
                    ctx.store_secret(TOKEN, &token).await?;
                    tracing::info!(instance = %ctx.instance(), "dirigera hub paired");
                    return Ok(Some(token));
                }
                Ok(Grant::NotYet) => {}
                Ok(Grant::Expired) => break,
                Ok(Grant::Refused(why)) => bail!("dirigera pairing refused: {why}"),
                Err(e) if is_pin_mismatch(&e) => {
                    park(ctx, pin_changed(config, ctx)).await?;
                    return Ok(None);
                }
                // A slow answer must not lose a press: ask again.
                Err(e) => {
                    tracing::debug!(instance = %ctx.instance(), error = %format!("{e:#}"), "dirigera token request failed");
                }
            }
        }
    }
}

/// Loads every device, publishes them and their current values.
async fn full_load(hub: &Hub, ctx: &DriverCtx) -> anyhow::Result<Index> {
    let (status, body) = hub.home().await?;
    if status == http::StatusCode::FORBIDDEN || status == http::StatusCode::UNAUTHORIZED {
        return Err(anyhow!(TokenRejected));
    }
    if !status.is_success() {
        bail!("dirigera home failed: {status}");
    }
    let json: Json = serde_json::from_slice(&body).context("dirigera home")?;
    let raw = json["devices"]
        .as_array()
        .context("no devices in the dirigera home")?;
    let Load {
        devices,
        mut index,
        skipped,
    } = model::load(ctx.instance(), raw);
    log_skipped(ctx, &skipped);

    let keep: Vec<DeviceId> = devices.iter().map(|d| d.id.clone()).collect();
    for id in ctx.devices() {
        if !keep.contains(&id) {
            ctx.remove_device(&id);
        }
    }
    let count = devices.len();
    for device in devices {
        ctx.upsert_device(device);
    }
    for item in raw {
        if let Some(update) = index.decode(item) {
            apply(ctx, &update);
        }
    }
    tracing::info!(instance = %ctx.instance(), devices = count, "dirigera hub loaded");
    Ok(index)
}

/// One line for what is left out: unknown types say so louder.
fn log_skipped(ctx: &DriverCtx, skipped: &[Skipped]) {
    let mut unknown: Vec<&str> = skipped
        .iter()
        .filter(|s| !s.known)
        .map(|s| s.device_type.as_str())
        .collect();
    unknown.sort_unstable();
    unknown.dedup();
    if !unknown.is_empty() {
        tracing::warn!(instance = %ctx.instance(), types = ?unknown, "dirigera device types unknown to the driver, left out");
    }
    let mut known: Vec<&str> = skipped
        .iter()
        .filter(|s| s.known)
        .map(|s| s.device_type.as_str())
        .collect();
    known.sort_unstable();
    known.dedup();
    if !known.is_empty() {
        tracing::debug!(instance = %ctx.instance(), types = ?known, "dirigera devices left out");
    }
}

fn apply(ctx: &DriverCtx, update: &Update) {
    let Some(device) = &update.device else {
        return;
    };
    for (key, value) in &update.values {
        ctx.set_state(device, key, value.clone());
    }
    if let Some(online) = update.online {
        ctx.set_availability(device, online);
    }
}

/// Applies one websocket message. Returns `true` when devices changed shape
/// (added, removed, renamed, moved): a full load is due.
fn apply_message(ctx: &DriverCtx, index: &mut Index, text: &str) -> bool {
    let Ok(message) = serde_json::from_str::<Json>(text) else {
        tracing::debug!("unparsable dirigera event");
        return false;
    };
    match index.classify(&message) {
        Event::State(data) => {
            if let Some(update) = index.decode(data) {
                apply(ctx, &update);
            }
            false
        }
        Event::Press(data) => {
            if let Some((device, pattern)) = index.press(data) {
                ctx.pulse_state(&device, "button", pattern);
            }
            false
        }
        Event::Reload => true,
        Event::Ignore => false,
    }
}

/// Owns the websocket: hands every message over, sends the ping event
/// every 30 s, and ends (closing the channel) when the hub closes the
/// connection or goes silent.
async fn read_events(mut socket: WebSocket, tx: mpsc::Sender<String>) {
    let mut pings = 0u64;
    let mut next_ping = Instant::now() + PING_EVERY;
    let mut heard = Instant::now();
    loop {
        if Instant::now() >= next_ping {
            pings += 1;
            let time = jiff::Timestamp::now().to_string();
            if let Err(e) = socket.send_text(&hub::ping(pings, &time)).await {
                tracing::debug!(error = %format!("{e:#}"), "dirigera ping failed");
                return;
            }
            next_ping = Instant::now() + PING_EVERY;
        }
        if heard.elapsed() > SILENCE {
            tracing::info!("dirigera event websocket silent, reconnecting");
            return;
        }
        let wait = next_ping
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(100));
        match socket.recv_text(wait).await {
            Ok(text) => {
                heard = Instant::now();
                if tx.send(text).await.is_err() {
                    return;
                }
            }
            Err(e) if e.is::<tokio::time::error::Elapsed>() => {}
            Err(e) => {
                tracing::info!(error = %format!("{e:#}"), "dirigera event websocket closed");
                return;
            }
        }
    }
}

/// Commands run beside the event loop: a slow hub never freezes events.
fn spawn_command(hub: &Arc<Hub>, index: &Index, command: CommandRequest) {
    let Some((device, body)) = index.encode(&command.device.id, &command.key, &command.value)
    else {
        command.reply(Err(moli_i18n::tr!("pilotes.dirigera.point_non_ecrivable")));
        return;
    };
    let hub = Arc::clone(hub);
    tokio::spawn(async move {
        let result = tokio::time::timeout(COMMAND_BUDGET, patch(&hub, &device, &body))
            .await
            .unwrap_or_else(|_| Err(moli_i18n::tr!("pilotes.dirigera.hub_muet")));
        command.reply(result);
    });
}

async fn patch(hub: &Hub, device: &str, body: &Json) -> Result<(), String> {
    let (status, answer) = hub
        .patch(device, body)
        .await
        .map_err(|e| format!("{e:#}"))?;
    // A 2xx is a success, with or without a body.
    if status.is_success() {
        Ok(())
    } else {
        Err(moli_i18n::tr!(
            "pilotes.dirigera.hub_repond",
            status = status,
            detail = hub::excerpt(&answer)
        ))
    }
}
