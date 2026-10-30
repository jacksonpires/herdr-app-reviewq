use crate::runner::{Cmd, Output, Runner};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct CreateReq {
    pub repo: PathBuf,
    pub branch: String,
    pub base: String,
    pub path: PathBuf,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Created {
    pub workspace_id: String,
    pub path: PathBuf,
}

/// Agente visto pelo herdr-core (`hc ls`), com o diretório onde ele roda.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentInfo {
    pub agent: String,
    pub cwd: Option<PathBuf>,
    pub foreground_cwd: Option<PathBuf>,
}

/// Aviso para o mod do herdr-app mostrar como toast (o herdr-app não tem notificação por CLI).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    pub at: chrono::DateTime<chrono::Utc>,
    pub title: String,
    pub body: String,
    pub sound: String,
    /// Space que o clique no toast mostra.
    #[serde(default)]
    pub space: Option<String>,
}

/// Quantos avisos o arquivo guarda; o mod só mostra os que ainda não viu.
const OUTBOX_KEEP: usize = 50;
/// Label que marca os terminais de um worktree do reviewq.
const WORKTREE_LABEL: &str = "reviewq.worktree";
/// Quanto esperar a janela do herdr-app adotar um space recém-criado.
const ADOPT_WAIT: Duration = Duration::from_secs(3);

pub trait Herdr {
    /// Cria o worktree (`git worktree add`, sem `--force`) e abre um space para ele.
    fn create_worktree(&self, req: &CreateReq) -> Result<Created>;
    /// Acha o space aberto para `path`; abre um, chamado `label`, se o worktree existe mas não há space.
    fn find_workspace(&self, repo: &Path, path: &Path, label: &str) -> Result<Option<String>>;
    /// Só consulta: devolve o space aberto para `path`, sem nunca abrir um.
    fn find_open_workspace(&self, repo: &Path, path: &Path) -> Result<Option<String>>;
    /// Fecha o space e encerra os terminais dele. Não mexe no worktree.
    fn close_workspace(&self, workspace_id: &str) -> Result<()>;
    fn notify(&self, title: &str, body: &str, sound: &str, space: Option<&str>) -> Result<()>;
    /// Mostra o space na janela do herdr-app.
    fn focus_workspace(&self, id: &str) -> Result<()>;
    /// Agentes (Claude, Codex…) que o herdr-core vê, com o diretório de cada um.
    fn list_agents(&self, deadline: Instant) -> Result<Vec<AgentInfo>>;
}

/// Id do space de um worktree: estável para o mesmo caminho, então o daemon e a CLI acham
/// o mesmo space sem guardar nada além do caminho.
pub fn space_id(path: &Path) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in canon(path).to_string_lossy().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("rq-{h:016x}")
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn json_of(out: &Output, what: &str) -> Result<Value> {
    let text = if out.stdout.trim().is_empty() { out.stderr.trim() } else { out.stdout.trim() };
    let v: Value = serde_json::from_str(text).with_context(|| format!("resposta inválida de {what}: {text}"))?;
    if let Some(err) = v.get("error") {
        bail!("{what}: {}", err.as_str().map(String::from).unwrap_or_else(|| err.to_string()));
    }
    if !out.success() {
        bail!("{what} falhou: {text}");
    }
    Ok(v)
}

/// Fala com o herdr-app (`herdr-app do`, só com a janela aberta) e com o herdr-core (`hc`,
/// sempre disponível). Spaces nascem pelo core, com labels, para funcionar com a janela fechada.
pub struct HerdrApp<'a> {
    runner: &'a dyn Runner,
    outbox: PathBuf,
    timeout: Duration,
}

impl<'a> HerdrApp<'a> {
    pub fn new(runner: &'a dyn Runner, outbox: PathBuf) -> Self {
        Self { runner, outbox, timeout: Duration::from_secs(30) }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn hc(&self) -> Cmd {
        Cmd::new("hc").timeout(self.timeout)
    }

    fn app(&self, command: &str, args: Value) -> Result<Value> {
        let cmd = Cmd::new("herdr-app").args(["do", command]).arg(args.to_string()).timeout(self.timeout);
        json_of(&self.runner.run(&cmd)?, &format!("herdr-app {command}"))
    }

    /// Endereços dos terminais vivos do space.
    fn space_terminals(&self, id: &str, timeout: Duration) -> Result<Vec<String>> {
        let cmd = self.hc().args(["ls", "--json", "--label"]).arg(format!("herdr.space={id}")).timeout(timeout);
        let ls = json_of(&self.runner.run(&cmd)?, "hc ls")?;
        Ok(ls
            .get("terminals")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|t| t.get("state").and_then(Value::as_str) == Some("live"))
            .filter_map(|t| t.get("address").and_then(Value::as_str).map(String::from))
            .collect())
    }

    /// Abre um shell no worktree num space novo. O nome é cosmético: com a janela fechada o
    /// space fica com o nome da pasta até ser renomeado.
    fn open_space(&self, path: &Path, label: &str) -> Result<String> {
        let id = space_id(path);
        let cmd = self
            .hc()
            .args(["run", "--bg", "--keep", "--no-parent", "--json", "--cwd"])
            .arg(path.to_string_lossy())
            .arg("--label")
            .arg(format!("herdr.space={id}"))
            .arg("--label")
            .arg(format!("{WORKTREE_LABEL}={}", canon(path).display()));
        json_of(&self.runner.run(&cmd)?, "hc run")?;
        if self.wait_in_window(&id) {
            let _ = self.app("space.rename", serde_json::json!({ "id": id, "name": label }));
        }
        Ok(id)
    }

    /// O herdr-app adota o terminal um instante depois do `hc run`; renomear ou mostrar o space
    /// antes disso falha com "no such space". Espera até `ADOPT_WAIT`; com a janela fechada,
    /// desiste na hora (o space aparece quando ela abrir).
    fn wait_in_window(&self, id: &str) -> bool {
        let until = Instant::now() + ADOPT_WAIT;
        loop {
            let Ok(out) = self.runner.run(&Cmd::new("herdr-app").arg("ls").timeout(self.timeout)) else { return false };
            let Ok(ls) = json_of(&out, "herdr-app ls") else { return false };
            let spaces = ls.get("spaces").and_then(Value::as_array).into_iter().flatten();
            if spaces.filter_map(|s| s.get("id").and_then(Value::as_str)).any(|s| s == id) {
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn append_notice(&self, notice: &Notice) -> Result<()> {
        let mut lines: Vec<String> = match std::fs::read_to_string(&self.outbox) {
            Ok(t) => t.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e.into()),
        };
        lines.push(serde_json::to_string(notice)?);
        let skip = lines.len().saturating_sub(OUTBOX_KEEP);
        if let Some(dir) = self.outbox.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.outbox.with_extension("tmp");
        std::fs::write(&tmp, lines[skip..].join("\n") + "\n")?;
        std::fs::rename(&tmp, &self.outbox)?;
        Ok(())
    }
}

impl Herdr for HerdrApp<'_> {
    fn create_worktree(&self, r: &CreateReq) -> Result<Created> {
        let cmd = Cmd::new("git")
            .cwd(r.repo.clone())
            .args(["worktree", "add", "-q", "-b"])
            .arg(r.branch.clone())
            .arg(r.path.to_string_lossy())
            .arg(r.base.clone())
            .timeout(Duration::from_secs(300));
        let out = self.runner.run(&cmd)?;
        if !out.success() {
            bail!("git worktree add falhou: {}", out.stderr.trim());
        }
        let id = self.open_space(&r.path, &r.label)?;
        Ok(Created { workspace_id: id, path: r.path.clone() })
    }

    fn find_workspace(&self, repo: &Path, path: &Path, label: &str) -> Result<Option<String>> {
        if let Some(id) = self.find_open_workspace(repo, path)? {
            return Ok(Some(id));
        }
        if !path.exists() {
            return Ok(None);
        }
        self.open_space(path, label).map(Some)
    }

    fn find_open_workspace(&self, _repo: &Path, path: &Path) -> Result<Option<String>> {
        let id = space_id(path);
        Ok((!self.space_terminals(&id, self.timeout)?.is_empty()).then_some(id))
    }

    fn close_workspace(&self, id: &str) -> Result<()> {
        let _ = self.app("space.close", serde_json::json!({ "id": id, "confirmed": true }));
        for t in self.space_terminals(id, self.timeout)? {
            let out = self.runner.run(&self.hc().args(["kill", &t]))?;
            if !out.success() {
                bail!("hc kill {t} falhou: {}", out.stderr.trim());
            }
        }
        Ok(())
    }

    fn notify(&self, title: &str, body: &str, sound: &str, space: Option<&str>) -> Result<()> {
        self.append_notice(&Notice {
            id: uuid::Uuid::new_v4().to_string(),
            at: chrono::Utc::now(),
            title: title.into(),
            body: body.into(),
            sound: sound.into(),
            space: space.map(String::from),
        })
    }

    fn focus_workspace(&self, id: &str) -> Result<()> {
        self.app("space.select", serde_json::json!({ "id": id })).map(|_| ())
    }

    fn list_agents(&self, deadline: Instant) -> Result<Vec<AgentInfo>> {
        let until = |deadline: Instant| -> Result<Duration> {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("prazo esgotado esperando o herdr-core");
            }
            Ok(left.min(self.timeout))
        };
        let ls = json_of(&self.runner.run(&self.hc().args(["ls", "--json"]).timeout(until(deadline)?))?, "hc ls")?;
        let mut out = vec![];
        for (address, mut agent) in crate::agents::parse_agents(&ls) {
            if !crate::agents::is_known(&agent.agent) {
                continue;
            }
            let get = self.hc().args(["get", &address, "--json"]).timeout(until(deadline)?);
            if let Ok(v) = json_of(&self.runner.run(&get)?, "hc get") {
                agent.foreground_cwd = crate::agents::foreground_cwd(&v);
            }
            out.push(agent);
        }
        Ok(out)
    }
}

/// Dublê para testes: cria worktrees com git de verdade, sem `--force`.
#[cfg(test)]
pub struct FakeHerdr {
    pub open: std::cell::RefCell<std::collections::BTreeMap<String, (PathBuf, PathBuf)>>,
    pub notifications: std::cell::RefCell<Vec<String>>,
    /// Space de cada aviso, na ordem.
    pub notified_spaces: std::cell::RefCell<Vec<Option<String>>>,
    /// Quando true, `notify` falha.
    pub fail_notify: std::cell::Cell<bool>,
    /// Ids passados a `focus_workspace`, em ordem.
    pub focus_calls: std::cell::RefCell<Vec<String>>,
    /// Ids passados a `close_workspace`, em ordem.
    pub closed: std::cell::RefCell<Vec<String>>,
    /// `focus_workspace` falha (janela do herdr-app fechada).
    pub fail_focus: std::cell::Cell<bool>,
    /// Resposta de `list_agents`.
    pub agents: std::cell::RefCell<Vec<AgentInfo>>,
    pub fail_agents: std::cell::Cell<bool>,
    next: std::cell::Cell<u32>,
}

#[cfg(test)]
impl Default for FakeHerdr {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl FakeHerdr {
    pub fn new() -> Self {
        Self {
            open: Default::default(),
            notifications: Default::default(),
            notified_spaces: Default::default(),
            fail_notify: std::cell::Cell::new(false),
            focus_calls: Default::default(),
            closed: Default::default(),
            fail_focus: Default::default(),
            agents: Default::default(),
            fail_agents: Default::default(),
            next: std::cell::Cell::new(1),
        }
    }

    fn git(dir: &Path, args: &[&str]) -> Result<()> {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output()?;
        if !out.status.success() {
            bail!("git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
        }
        Ok(())
    }

    fn open_space(&self, repo: &Path, path: &Path) -> String {
        let id = format!("w{}", self.next.get());
        self.next.set(self.next.get() + 1);
        self.open.borrow_mut().insert(id.clone(), (repo.to_path_buf(), path.to_path_buf()));
        id
    }
}

#[cfg(test)]
impl Herdr for FakeHerdr {
    fn create_worktree(&self, r: &CreateReq) -> Result<Created> {
        Self::git(&r.repo, &["worktree", "add", "-q", "-b", &r.branch, &r.path.to_string_lossy(), &r.base])?;
        Ok(Created { workspace_id: self.open_space(&r.repo, &r.path), path: r.path.clone() })
    }

    fn find_workspace(&self, repo: &Path, path: &Path, _label: &str) -> Result<Option<String>> {
        if let Some(id) = self.find_open_workspace(repo, path)? {
            return Ok(Some(id));
        }
        Ok(path.exists().then(|| self.open_space(repo, path)))
    }

    fn find_open_workspace(&self, _repo: &Path, path: &Path) -> Result<Option<String>> {
        let want = canon(path);
        Ok(self.open.borrow().iter().find(|(_, (_, p))| canon(p) == want).map(|(id, _)| id.clone()))
    }

    fn close_workspace(&self, id: &str) -> Result<()> {
        self.open.borrow_mut().remove(id).context("space desconhecido")?;
        self.closed.borrow_mut().push(id.into());
        Ok(())
    }

    fn notify(&self, title: &str, body: &str, _sound: &str, space: Option<&str>) -> Result<()> {
        if self.fail_notify.get() {
            bail!("aviso falhou (teste)");
        }
        self.notifications.borrow_mut().push(format!("{title} | {body}"));
        self.notified_spaces.borrow_mut().push(space.map(String::from));
        Ok(())
    }

    fn focus_workspace(&self, id: &str) -> Result<()> {
        if self.fail_focus.get() {
            bail!("herdr-app space.select: janela fechada (teste)");
        }
        self.focus_calls.borrow_mut().push(id.into());
        Ok(())
    }

    fn list_agents(&self, deadline: Instant) -> Result<Vec<AgentInfo>> {
        if Instant::now() >= deadline {
            bail!("prazo esgotado esperando o herdr-core");
        }
        if self.fail_agents.get() {
            bail!("hc ls falhou (teste)");
        }
        Ok(self.agents.borrow().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::FakeRunner;

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    fn app<'a>(fake: &'a FakeRunner, dir: &Path) -> HerdrApp<'a> {
        HerdrApp::new(fake, dir.join("notifications.jsonl"))
    }

    fn listed(ids: &[&str]) -> String {
        serde_json::json!({ "spaces": ids.iter().map(|id| serde_json::json!({ "id": id })).collect::<Vec<_>>() }).to_string()
    }

    #[test]
    fn space_id_is_stable_per_path_and_distinct_between_paths() {
        assert_eq!(space_id(Path::new("/w/o/r/pr-7")), space_id(Path::new("/w/o/r/pr-7")));
        assert_ne!(space_id(Path::new("/w/o/r/pr-7")), space_id(Path::new("/w/o/r/pr-70")));
        assert!(space_id(Path::new("/w/o/r/pr-7")).starts_with("rq-"));
    }

    #[test]
    fn create_adds_worktree_without_force_then_opens_labeled_space_and_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        fake.on("git worktree add", Output::ok(""));
        fake.on("hc run", Output::ok(r#"{"address":"t9","state":"live"}"#));
        fake.on("herdr-app ls", Output::ok(&listed(&[&space_id(Path::new("/w/o/r/pr-7"))])));
        fake.on("space.rename", Output::ok(r#"{"ok":true}"#));
        let label = "#7 Ajusta \"webhook\" de fornecedores — ação";
        let created = app(&fake, dir.path())
            .create_worktree(&CreateReq {
                repo: "/r".into(), branch: "feat/x".into(), base: "origin/feat/x".into(),
                path: "/w/o/r/pr-7".into(), label: label.into(),
            })
            .unwrap();
        assert_eq!(created.workspace_id, space_id(Path::new("/w/o/r/pr-7")));
        let calls = fake.calls.borrow();
        assert_eq!(calls[0].args, ["worktree", "add", "-q", "-b", "feat/x", "/w/o/r/pr-7", "origin/feat/x"]);
        assert_eq!(calls[0].cwd.as_deref(), Some(Path::new("/r")));
        assert!(!calls.iter().any(|c| c.args.iter().any(|a| a == "--force")));
        assert!(calls[1].args.contains(&format!("herdr.space={}", created.workspace_id)));
        assert!(calls[1].args.contains(&"reviewq.worktree=/w/o/r/pr-7".to_string()));
        assert!(calls[1].args.contains(&"--no-parent".to_string()) && calls[1].args.contains(&"--keep".to_string()));
        let rename: Value = serde_json::from_str(calls[3].args.last().unwrap()).unwrap();
        assert_eq!(rename["name"], label);
    }

    #[test]
    fn create_survives_closed_window_without_naming_the_space() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        fake.on("git worktree add", Output::ok(""));
        fake.on("hc run", Output::ok(r#"{"address":"t9"}"#));
        fake.on("herdr-app ls", Output::fail(1, r#"{"error":"Herdr is not running"}"#));
        let req = CreateReq { repo: "/r".into(), branch: "b".into(), base: "origin/b".into(), path: "/w/pr-1".into(), label: "#1".into() };
        assert!(app(&fake, dir.path()).create_worktree(&req).is_ok());
        assert!(fake.lines().iter().all(|l| !l.contains("space.rename")));
    }

    #[test]
    fn names_the_space_only_after_the_window_adopts_it() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        let path = dir.path().join("pr-1");
        std::fs::create_dir_all(&path).unwrap();
        fake.on("hc ls", Output::ok(r#"{"terminals":[]}"#));
        fake.on("hc run", Output::ok(r#"{"address":"t4"}"#));
        fake.on("herdr-app ls", Output::ok(&listed(&["outro"])));
        fake.on("herdr-app ls", Output::ok(&listed(&["outro", &space_id(&path)])));
        fake.on("space.rename", Output::ok(r#"{"ok":true}"#));
        app(&fake, dir.path()).find_workspace(Path::new("/r"), &path, "#1 x").unwrap();
        let lines = fake.lines();
        assert_eq!(lines.iter().filter(|l| l.starts_with("herdr-app ls")).count(), 2);
        assert!(lines.last().unwrap().contains("space.rename"));
    }

    #[test]
    fn create_fails_without_opening_space_when_git_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        fake.on("git worktree add", Output::fail(128, "fatal: a branch named 'b' already exists"));
        let req = CreateReq { repo: "/r".into(), branch: "b".into(), base: "origin/b".into(), path: "/w/pr-1".into(), label: "#1".into() };
        let err = app(&fake, dir.path()).create_worktree(&req).unwrap_err();
        assert!(format!("{err:#}").contains("already exists"));
        assert_eq!(fake.calls.borrow().len(), 1);
    }

    #[test]
    fn find_open_workspace_never_opens_a_space() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        fake.on("hc ls", Output::ok(r#"{"terminals":[{"address":"t3","state":"live"}]}"#));
        fake.on("hc ls", Output::ok(r#"{"terminals":[{"address":"t3","state":"finished"}]}"#));
        let h = app(&fake, dir.path());
        let path = dir.path().join("pr-1");
        assert_eq!(h.find_open_workspace(Path::new("/r"), &path).unwrap(), Some(space_id(&path)));
        assert_eq!(h.find_open_workspace(Path::new("/r"), &path).unwrap(), None);
        assert!(fake.lines()[0].contains(&format!("--label herdr.space={}", space_id(&path))));
        assert_eq!(fake.calls.borrow().len(), 2);
    }

    #[test]
    fn find_workspace_opens_space_only_when_worktree_exists() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        let existing = dir.path().join("pr-1");
        std::fs::create_dir_all(&existing).unwrap();
        fake.on("hc ls", Output::ok(r#"{"terminals":[]}"#));
        fake.on("hc run", Output::ok(r#"{"address":"t4"}"#));
        fake.on("herdr-app ls", Output::ok(&listed(&[&space_id(&existing)])));
        fake.on("space.rename", Output::ok(r#"{"ok":true}"#));
        fake.on("hc ls", Output::ok(r#"{"terminals":[]}"#));
        let h = app(&fake, dir.path());
        assert_eq!(h.find_workspace(Path::new("/r"), &existing, "#1 x").unwrap(), Some(space_id(&existing)));
        assert_eq!(h.find_workspace(Path::new("/r"), &dir.path().join("sumiu"), "#2").unwrap(), None);
        assert!(!fake.lines().last().unwrap().contains("hc run"));
    }

    #[test]
    fn close_kills_every_live_terminal_of_the_space_even_with_window_closed() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        fake.on("space.close", Output::fail(1, r#"{"error":"Herdr is not running"}"#));
        fake.on("hc ls", Output::ok(r#"{"terminals":[{"address":"t3","state":"live"},{"address":"t5","state":"live"}]}"#));
        fake.on("hc kill t3", Output::ok(""));
        fake.on("hc kill t5", Output::ok(""));
        app(&fake, dir.path()).close_workspace("rq-1").unwrap();
        let lines = fake.lines();
        assert!(lines[1].contains("--label herdr.space=rq-1"));
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn close_reports_kill_failure() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        fake.on("space.close", Output::ok(r#"{"ok":true}"#));
        fake.on("hc ls", Output::ok(r#"{"terminals":[{"address":"t3","state":"live"}]}"#));
        fake.on("hc kill t3", Output::fail(1, "no such terminal"));
        assert!(app(&fake, dir.path()).close_workspace("rq-1").is_err());
    }

    #[test]
    fn focus_reports_app_error() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        fake.on("space.select", Output::ok(r#"{"ok":true}"#));
        fake.on("space.select", Output::fail(1, r#"{"error":"no space rq-9"}"#));
        let h = app(&fake, dir.path());
        h.focus_workspace("rq-1").unwrap();
        assert!(format!("{:#}", h.focus_workspace("rq-9").unwrap_err()).contains("no space rq-9"));
        let sent: Value = serde_json::from_str(fake.calls.borrow()[0].args.last().unwrap()).unwrap();
        assert_eq!(sent["id"], "rq-1");
    }

    #[test]
    fn notify_appends_to_outbox_and_keeps_only_the_latest() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        let h = app(&fake, dir.path());
        for i in 0..(OUTBOX_KEEP + 3) {
            h.notify(&format!("#{i} pronto"), "título", "request", Some("rq-1")).unwrap();
        }
        let text = std::fs::read_to_string(dir.path().join("notifications.jsonl")).unwrap();
        let notices: Vec<Notice> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(notices.len(), OUTBOX_KEEP);
        assert_eq!(notices[0].title, "#3 pronto");
        assert_eq!(notices.last().unwrap().space.as_deref(), Some("rq-1"));
        assert!(fake.calls.borrow().is_empty());
    }

    #[test]
    fn list_agents_reads_foreground_cwd_of_known_agents_only() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        fake.on("hc ls", Output::ok(r#"{"terminals":[
            {"address":"t2","agent":"claude-code","cwd":"/w/pr-1","state":"live"},
            {"address":"t3","agent":"zsh","cwd":"/w/pr-2","state":"live"}]}"#));
        fake.on("hc get t2", Output::ok(r#"{"address":"t2","foreground":{"cwd":"/w/pr-1/app"}}"#));
        let agents = app(&fake, dir.path()).list_agents(far()).unwrap();
        assert_eq!(
            agents,
            vec![AgentInfo { agent: "claude".into(), cwd: Some("/w/pr-1".into()), foreground_cwd: Some("/w/pr-1/app".into()) }]
        );
        assert_eq!(fake.calls.borrow().len(), 2);
    }

    #[test]
    fn list_agents_respects_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeRunner::new();
        assert!(app(&fake, dir.path()).list_agents(Instant::now()).is_err());
        assert!(fake.calls.borrow().is_empty());
    }
}
