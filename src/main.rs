use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use herdr_app_reviewq::config::{grace_duration, Config, DEFAULT_REMOVE_GRACE_SECS};
use herdr_app_reviewq::herdr::{Herdr, HerdrApp};
use herdr_app_reviewq::panel::{self, Sent, UiFacts, UiState, View, ViewConfig};
use herdr_app_reviewq::paths::Paths;
use herdr_app_reviewq::requests::{self, RequestKind};
use herdr_app_reviewq::runner::RealRunner;
use herdr_app_reviewq::state::{PrKey, State};
use herdr_app_reviewq::{daemon, service, status, ui};
use std::time::Duration;

/// Timeout de cada chamada ao herdr-app/hc de um atalho (quem apertou está esperando).
const SHORTCUT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Parser)]
#[command(name = "herdr-app-reviewq", version, about = "Worktrees prontos para os reviews pedidos a você, no herdr-app")]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Roda o reconciliador (usado pelo launchd)
    Daemon,
    /// Mostra o estado atual
    Status,
    /// Envia um pedido ao daemon
    Request {
        kind: Kind,
        /// PR no formato owner/repo#n
        #[arg(long)]
        pr: Option<String>,
        /// Imprime só o id do pedido, em JSON
        #[arg(long)]
        json: bool,
    },
    /// Gerencia o LaunchAgent
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Visão do painel em JSON (consumida pelo mod do herdr-app)
    Panel {
        /// Último pedido enviado pelo painel, para mostrar o resultado: id
        #[arg(long, requires_all = ["sent_kind", "sent_at"])]
        sent_id: Option<String>,
        #[arg(long)]
        sent_kind: Option<Kind>,
        #[arg(long)]
        sent_pr: Option<String>,
        /// Quando o pedido foi enviado (RFC 3339)
        #[arg(long)]
        sent_at: Option<String>,
    },
    /// Mostra o space de um PR no herdr-app
    Focus {
        #[command(subcommand)]
        target: FocusTarget,
    },
}

#[derive(Subcommand)]
enum FocusTarget {
    /// PR pronto há mais tempo
    FirstReady,
    /// Um PR específico (abre um space se o worktree não tiver um)
    Pr {
        /// PR no formato owner/repo#n
        #[arg(long)]
        pr: String,
    },
}

fn load_cfg(p: &Paths) -> Result<Config> {
    Config::load(&p.config_file, &p.home)
}

#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    Sync,
    Retry,
    Release,
    Adopt,
}

impl From<Kind> for RequestKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Sync => RequestKind::Sync,
            Kind::Retry => RequestKind::Retry,
            Kind::Release => RequestKind::Release,
            Kind::Adopt => RequestKind::Adopt,
        }
    }
}

#[derive(Subcommand)]
enum ServiceAction {
    Install,
    Uninstall,
    Restart,
}

fn panel_json(paths: &Paths, sent: Option<Sent>) -> Result<serde_json::Value> {
    let cfg = load_cfg(paths)?;
    let state = match paths.state_file().exists() {
        true => Some(State::read(&paths.state_file())?),
        false => None,
    };
    let facts = UiFacts { missing_worktrees: state.as_ref().map(panel::missing_worktrees).unwrap_or_default() };
    let ui = UiState { sent: sent.into_iter().collect(), ..Default::default() };
    let view = View::build(state.as_ref(), &ui, &ViewConfig::of(&cfg), &facts, chrono::Utc::now());
    let mut json = serde_json::to_value(&view)?;
    for (row, out) in view.rows.iter().zip(json["rows"].as_array_mut().into_iter().flatten()) {
        out["key"] = row.id.key().to_string().into();
        if let Some(k) = row.id.review() {
            out["log"] = paths.setup_log(k).to_string_lossy().into_owned().into();
        }
    }
    json["state_dir"] = paths.state_dir.to_string_lossy().into_owned().into();
    json["notifications"] = paths.notifications_file().to_string_lossy().into_owned().into();
    Ok(json)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = Paths::from_env()?;
    match cli.cmd {
        Command::Daemon => daemon::run(paths),
        Command::Status => {
            let state = State::read(&paths.state_file())?;
            let (grace, repos, poll) = match Config::load(&paths.config_file, &paths.home) {
                Ok(c) => (c.remove_grace_secs, c.repos.iter().map(|r| r.name.clone()).collect::<Vec<_>>(), c.poll_interval_secs),
                Err(_) => (DEFAULT_REMOVE_GRACE_SECS, Vec::new(), 60),
            };
            print!("{}", status::render_at(&state, chrono::Utc::now(), grace_duration(grace), &repos, poll));
            Ok(())
        }
        Command::Request { kind, pr, json } => {
            let id = requests::write(&paths.requests_dir(), kind.into(), pr)?;
            match json {
                true => println!("{}", serde_json::json!({ "id": id })),
                false => println!("pedido {id} enviado; veja o resultado com `herdr-app-reviewq status`"),
            }
            Ok(())
        }
        Command::Service { action } => match action {
            ServiceAction::Install => service::install(&paths),
            ServiceAction::Uninstall => service::uninstall(&paths),
            ServiceAction::Restart => service::restart(),
        },
        Command::Panel { sent_id, sent_kind, sent_pr, sent_at } => {
            let sent = match (sent_id, sent_kind, sent_at) {
                (Some(id), Some(kind), Some(at)) => Some(Sent {
                    id,
                    kind: kind.into(),
                    pr: sent_pr.as_deref().and_then(PrKey::parse),
                    at: chrono::DateTime::parse_from_rfc3339(&at).context("--sent-at inválido")?.with_timezone(&chrono::Utc),
                }),
                _ => None,
            };
            println!("{}", panel_json(&paths, sent)?);
            Ok(())
        }
        Command::Focus { target } => {
            let runner = RealRunner;
            let herdr = HerdrApp::new(&runner, paths.notifications_file()).with_timeout(SHORTCUT_TIMEOUT);
            let r = load_cfg(&paths).and_then(|cfg| match target {
                FocusTarget::FirstReady => match ui::focus_first_ready(&herdr, &cfg, || State::read(&paths.state_file()))? {
                    Some(_) => Ok(()),
                    None => herdr.notify("reviewq", "nenhum PR pronto", "none", None),
                },
                FocusTarget::Pr { pr } => {
                    let key = PrKey::parse(&pr).with_context(|| format!("PR inválido: {pr} (use owner/repo#n)"))?;
                    ui::focus_pr(&herdr, &cfg, &State::read(&paths.state_file())?, &key)
                }
            });
            ui::notify_error(&herdr, r)
        }
    }
}
