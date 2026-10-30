use crate::config::Config;
use crate::herdr::Herdr;
use crate::state::{Ownership, Phase, PrKey, Remote, State};
use anyhow::{bail, Context, Result};

pub const UI_LABEL: &str = "reviewq";

/// Erro de um atalho (`focus first-ready`, `focus pr`) também vira aviso no herdr-app: quem
/// apertou a tecla não vê o stderr. Se o aviso falhar, o erro original segue igual.
pub fn notify_error<T>(herdr: &dyn Herdr, r: Result<T>) -> Result<T> {
    if let Err(e) = &r {
        let _ = herdr.notify(UI_LABEL, &format!("{e:#}"), "none", None);
    }
    r
}

fn is_first_ready_candidate(r: &crate::state::PrRecord) -> bool {
    r.remote == Remote::Pending && r.ownership == Ownership::Managed && r.phase == Phase::Ready
}

pub fn pick_first_ready(state: &State) -> Option<PrKey> {
    state
        .prs
        .values()
        .filter(|r| is_first_ready_candidate(r))
        .min_by(|a, b| (a.first_seen_at, &a.key).cmp(&(b.first_seen_at, &b.key)))
        .map(|r| r.key.clone())
}

/// `focus first-ready`: escolhe no estado lido e, logo antes de focar, relê o estado e confere
/// que o escolhido ainda é gerenciado, pendente e pronto. Se deixou de ser, escolhe de novo (uma
/// vez) no estado relido. `Ok(None)` quando nenhum está pronto.
pub fn focus_first_ready(herdr: &dyn Herdr, cfg: &Config, read_state: impl Fn() -> Result<State>) -> Result<Option<PrKey>> {
    let Some(key) = pick_first_ready(&read_state()?) else { return Ok(None) };
    let fresh = read_state()?;
    let key = if fresh.get(&key).is_some_and(is_first_ready_candidate) {
        key
    } else {
        match pick_first_ready(&fresh) {
            Some(k) => k,
            None => return Ok(None),
        }
    };
    focus_pr(herdr, cfg, &fresh, &key)?;
    Ok(Some(key))
}

/// Mostra o space de um PR, abrindo um se não houver. Nunca muta estado.
pub fn focus_pr(herdr: &dyn Herdr, cfg: &Config, state: &State, key: &PrKey) -> Result<()> {
    let rec = state.get(key).with_context(|| format!("{key} não está mais no painel"))?;
    if matches!(rec.phase, Phase::Removing | Phase::Creating) {
        bail!("{key} está sendo criado ou removido");
    }
    if !rec.path.exists() {
        bail!("o worktree de {key} não existe mais");
    }
    let repo = cfg.repo(&key.repo).with_context(|| format!("repo {} fora da config", key.repo))?;
    let label = format!("#{} {}", key.number, rec.title);
    let ws = herdr
        .find_workspace(&repo.path, &rec.path, &label)?
        .with_context(|| format!("não consegui abrir um space para o worktree de {key}"))?;
    herdr.focus_workspace(&ws)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::FakeHerdr;
    use crate::state::{Phase, PrRecord, State};

    fn focused(h: &FakeHerdr) -> Option<String> {
        h.focus_calls.borrow().last().cloned()
    }

    #[test]
    fn notify_error_shows_failure_and_keeps_it() {
        let h = FakeHerdr::new();
        assert!(notify_error(&h, Ok(())).is_ok());
        assert!(h.notifications.borrow().is_empty(), "sucesso não notifica");
        let r = notify_error::<()>(&h, Err(anyhow::anyhow!("herdr-app fechado")).context("focar o PR"));
        assert_eq!(format!("{:#}", r.unwrap_err()), "focar o PR: herdr-app fechado");
        assert_eq!(*h.notifications.borrow(), vec!["reviewq | focar o PR: herdr-app fechado".to_string()]);
        h.fail_notify.set(true);
        assert!(notify_error::<()>(&h, Err(anyhow::anyhow!("x"))).is_err());
    }

    #[test]
    fn first_ready_is_oldest_managed_pending_ready() {
        let mut s = State::default();
        let mut a = PrRecord::fixture("o/r", 2, "x", "s");
        a.first_seen_at = chrono::Utc::now();
        let b = PrRecord::fixture("o/r", 3, "x", "s"); // epoch: mais antigo
        let mut c = PrRecord::fixture("o/r", 1, "x", "s");
        c.phase = Phase::Preparing;
        for r in [a, b, c] {
            s.insert(r);
        }
        assert_eq!(pick_first_ready(&s), Some(crate::state::PrKey::new("o/r", 3)));
        assert_eq!(pick_first_ready(&State::default()), None);
    }

    #[test]
    fn focus_first_ready_revalidates_on_fresh_state() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let mut rec = PrRecord::fixture("o/r", 7, "x", "s");
        rec.path = dir.path().to_path_buf();
        let mut ready = State::default();
        ready.insert(rec.clone());
        // nenhum pronto
        let h = FakeHerdr::new();
        assert_eq!(focus_first_ready(&h, &c, || Ok(State::default())).unwrap(), None);

        // escolhido no primeiro estado, mas deixou de estar pronto no estado relido
        let h = FakeHerdr::new();
        h.open.borrow_mut().insert("w42".into(), ("/repo".into(), rec.path.clone()));
        let mut gone = State::default();
        let mut adopted = rec.clone();
        adopted.ownership = crate::state::Ownership::Adopted { reason: "x".into(), at: chrono::Utc::now() };
        gone.insert(adopted);
        let reads = std::cell::RefCell::new(vec![gone.clone(), ready.clone()]);
        assert_eq!(focus_first_ready(&h, &c, || Ok(reads.borrow_mut().pop().unwrap())).unwrap(), None, "nenhum pronto no estado relido");
        assert!(focused(&h).is_none());

        // deixou de estar pronto, mas outro ficou pronto: escolhe de novo no estado relido
        let mut other = PrRecord::fixture("o/r", 9, "x", "s");
        other.path = dir.path().join("wt9");
        std::fs::create_dir_all(&other.path).unwrap();
        h.open.borrow_mut().insert("w99".into(), ("/repo".into(), other.path.clone()));
        let mut regrown = gone;
        regrown.insert(other);
        let reads = std::cell::RefCell::new(vec![regrown, ready.clone()]);
        let got = focus_first_ready(&h, &c, || Ok(reads.borrow_mut().pop().unwrap())).unwrap();
        assert_eq!(got, Some(crate::state::PrKey::new("o/r", 9)));
        assert_eq!(focused(&h).as_deref(), Some("w99"));

        // ainda pronto: foca
        let reads = std::cell::RefCell::new(vec![ready.clone(), ready]);
        let got = focus_first_ready(&h, &c, || Ok(reads.borrow_mut().pop().unwrap())).unwrap();
        assert_eq!(got, Some(crate::state::PrKey::new("o/r", 7)));
        assert_eq!(focused(&h).as_deref(), Some("w42"));
    }

    fn cfg(home: &std::path::Path) -> Config {
        Config::parse("worktrees_dir = \"/w\"\n[[repos]]\nname = \"o/r\"\npath = \"/repo\"\nsetup = []\n", home).unwrap()
    }

    #[test]
    fn focus_pr_focuses_space_of_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let h = FakeHerdr::new();
        let mut rec = PrRecord::fixture("o/r", 7, "x", "s");
        rec.path = dir.path().to_path_buf();
        h.open.borrow_mut().insert("w42".into(), ("/repo".into(), rec.path.clone()));
        let mut s = State::default();
        s.insert(rec);
        let key = crate::state::PrKey::new("o/r", 7);
        focus_pr(&h, &cfg(dir.path()), &s, &key).unwrap();
        assert_eq!(focused(&h).as_deref(), Some("w42"));
    }

    #[test]
    fn focus_pr_opens_space_when_worktree_has_none() {
        let dir = tempfile::tempdir().unwrap();
        let h = FakeHerdr::new();
        let mut rec = PrRecord::fixture("o/r", 7, "x", "s");
        rec.path = dir.path().to_path_buf();
        let mut s = State::default();
        s.insert(rec);
        focus_pr(&h, &cfg(dir.path()), &s, &crate::state::PrKey::new("o/r", 7)).unwrap();
        assert_eq!(h.open.borrow().len(), 1);
        assert_eq!(focused(&h), h.open.borrow().keys().next().cloned());
    }

    #[test]
    fn focus_pr_refuses_unusable_records() {
        let dir = tempfile::tempdir().unwrap();
        let h = FakeHerdr::new();
        let c = cfg(dir.path());
        let mut s = State::default();
        let key = crate::state::PrKey::new("o/r", 7);
        assert!(focus_pr(&h, &c, &s, &key).unwrap_err().to_string().contains("não está mais"));

        let mut rec = PrRecord::fixture("o/r", 7, "x", "s");
        rec.path = dir.path().to_path_buf();
        rec.phase = Phase::Removing;
        s.insert(rec.clone());
        assert!(focus_pr(&h, &c, &s, &key).unwrap_err().to_string().contains("removido"));

        rec.phase = Phase::Ready;
        rec.path = dir.path().join("sumiu");
        s.insert(rec);
        assert!(focus_pr(&h, &c, &s, &key).unwrap_err().to_string().contains("não existe mais"));
        assert!(focused(&h).is_none());
    }

    #[test]
    fn focus_pr_reports_closed_window() {
        let dir = tempfile::tempdir().unwrap();
        let h = FakeHerdr::new();
        h.fail_focus.set(true);
        let mut rec = PrRecord::fixture("o/r", 7, "x", "s");
        rec.path = dir.path().to_path_buf();
        let mut s = State::default();
        s.insert(rec);
        let err = focus_pr(&h, &cfg(dir.path()), &s, &crate::state::PrKey::new("o/r", 7)).unwrap_err();
        assert!(format!("{err:#}").contains("janela fechada"));
    }
}
