import { View, div } from "gpui-kit";
import { h_flex, v_flex } from "gpui-base";
import { exec, home, readTextFile, watchFolder, shared, setShared, props, openOverlay, closeOverlay, notify, playSound, run, theme } from "herdr";

const BIN = () => `${home()}/.local/bin/herdr-app-reviewq`;
const SEEN_KEY = "reviewq.notices.seen";
// Refresh even without file changes, so "sync 2s" and "há 3min" keep moving.
const TICK = 30e3;
// Setup logs change all the time while a worktree prepares; only these files change the panel.
const WATCHED = ["/state.json", "/notifications.jsonl"];
const SOUND = { done: "done", request: "blocked" };

const SECTION = {
  started: "Iniciados", pending: "Pendentes", mine: "Meus PRs",
  adopted: "Adotados", no_sync: "Sem sync", leaving: "Saindo",
};
const KIND = { sync: "sync", retry: "tentar de novo", release: "liberar", adopt: "adotar" };
const NOT_ELIGIBLE = {
  retry: "tentar de novo só vale para setup que falhou ou está bloqueado sem worktree",
  adopt: "só dá para adotar um worktree gerenciado pelo daemon",
  release: "só dá para liberar um worktree adotado",
};
const FOOTER = "↑↓ mover · enter abrir · o GitHub · l log · s sync · R tentar de novo · a adotar · r liberar · esc fechar";

function confirmText(kind, key) {
  if (kind === "adopt") return `adotar ${key}? protege da limpeza automática e cancela o setup em andamento (y/n)`;
  if (kind === "release") return `liberar ${key}? remove o worktree; se o PR ainda estiver pendente, ele só volta com um novo pedido de review (y/n)`;
  return `${KIND[kind]} ${key}? (y/n)`;
}

async function loadView(sent) {
  const args = ["panel"];
  if (sent) {
    args.push("--sent-id", sent.id, "--sent-kind", sent.kind, "--sent-at", sent.at);
    if (sent.pr) args.push("--sent-pr", sent.pr);
  }
  const r = await exec(BIN(), args);
  if (r.code !== 0) throw new Error(r.stderr.trim() || `herdr-app-reviewq panel saiu com ${r.code}`);
  return JSON.parse(r.stdout);
}

export default class Reviewq extends View {
  init(_props, cx) {
    this.view = null;
    this.error = null;
    this.selected = null;
    this.confirm = null;
    this.message = null;
    this.sent = null;
    this.width = 0;
    this.isPanel = Boolean(props()?.panel || props()?.overlay);
    if (this.isPanel) {
      this.handle = cx.focus_handle();
      this.handle.focus();
      cx.timer.after(100, () => this.handle.focus());
    }
    this.watching = false;
    cx.spawn(async (cx) => this.reload(cx));
    cx.timer.every(TICK, (cx) => cx.spawn(async (cx) => this.reload(cx)));
  }

  // The folder is only known after the first successful load (it comes from the binary), so a
  // copy that started before the binary was installed begins watching on a later tick.
  async watch(cx) {
    if (!this.isPanel) await this.deliverNotices(true);
    for (;;) {
      const changed = await watchFolder(this.view.state_dir);
      if (!changed.some((p) => WATCHED.some((name) => p.endsWith(name)))) continue;
      await this.reload(cx);
      if (!this.isPanel) await this.deliverNotices(false);
    }
  }

  async reload(cx) {
    try {
      this.view = await loadView(this.sent);
      this.error = null;
      if (!this.watching) {
        this.watching = true;
        cx.spawn(async (cx) => this.watch(cx));
      }
    } catch (e) {
      this.error = String(e?.message ?? e);
    }
    cx.notify();
  }

  // Toasts for what the daemon wrote to notifications.jsonl since the last one shown. On the very
  // first run, everything already there counts as seen, so an install doesn't replay old notices.
  async deliverNotices(first) {
    const text = await readTextFile(this.view.notifications).catch(() => null);
    const notices = (text ?? "").split("\n").filter(Boolean).map((l) => { try { return JSON.parse(l); } catch { return null; } }).filter(Boolean);
    const seen = shared(SEEN_KEY);
    const latest = notices.at(-1)?.at;
    if (first && !seen) {
      if (latest) setShared(SEEN_KEY, latest);
      return;
    }
    const fresh = notices.filter((n) => !seen || n.at > seen);
    for (const n of fresh) {
      notify({
        title: n.title,
        body: n.body,
        level: n.title.startsWith("reviewq") ? "warning" : "info",
        action: n.space ? { name: "space.select", args: { id: n.space } } : undefined,
      });
      if (SOUND[n.sound]) playSound(SOUND[n.sound]);
    }
    if (fresh.length) setShared(SEEN_KEY, fresh.at(-1).at);
  }

  render(cx) {
    return this.isPanel ? this.panel(cx) : this.bar();
  }

  openPanel() {
    openOverlay({ mod: "reviewq", kind: "dialog", width: 0.85, height: 0.8, props: { panel: true } });
  }

  bar() {
    const t = theme();
    const v = this.view;
    const ready = v?.rows.filter((r) => r.id.review && r.status.startsWith("pronto")).length ?? 0;
    const stale = !v || v.header.stale || v.header.errors.length > 0 || this.error;
    return h_flex().id("root").size_full().px(12).gap(10).items_center().bg(t.chrome).text_size(11.8)
      .when(props()?.border === "top", (el) => el.border_t(1).border_color(t.border))
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click(() => this.openPanel())
      .tooltip(this.error ?? (v ? `${v.header.sync} · ${v.today_detail}` : "carregando reviewq"))
      .child(div().text_color(stale ? t.blocked : t.dim).child("reviewq"))
      .child(div().text_color(t.muted).child(v ? `${v.pending} pendentes` : "…"))
      .when(ready > 0, (el) => el.child(div().text_color(t.done).child(`${ready} prontos`)))
      .when(Boolean(v), (el) => el.child(div().text_color(t.dim).child(`${v.today} hoje`)));
  }

  rows() {
    return this.view?.rows ?? [];
  }

  selectedRow() {
    const rows = this.rows();
    return rows.find((r) => r.key === this.selected?.key && Boolean(r.id.review) === this.selected?.review) ?? rows[0];
  }

  select(row) {
    this.selected = row ? { key: row.key, review: Boolean(row.id.review) } : null;
  }

  close() {
    const id = props()?.overlay;
    if (id) closeOverlay(id);
  }

  async request(kind, pr, cx) {
    const args = ["request", kind, "--json"];
    if (pr) args.push("--pr", pr);
    const r = await exec(BIN(), args);
    if (r.code !== 0) {
      this.message = `pedido ${KIND[kind]} falhou: ${r.stderr.trim()}`;
    } else {
      this.sent = { id: JSON.parse(r.stdout).id, kind, pr, at: new Date().toISOString() };
    }
    await this.reload(cx);
  }

  async focusPr(row, cx) {
    const r = await exec(BIN(), ["focus", "pr", "--pr", row.key]);
    if (r.code === 0) return this.close();
    this.message = r.stderr.trim() || "não consegui abrir o PR";
    cx.notify();
  }

  showLog(row) {
    run("tab.new", { name: `log ${row.label}`, argv: ["less", "+G", row.log] });
    this.close();
  }

  onKey(e, cx) {
    const k = e.key;
    const rows = this.rows();
    const row = this.selectedRow();

    if (this.confirm) {
      const { kind, row: target } = this.confirm;
      if (k === "y") {
        this.confirm = null;
        const fresh = rows.find((r) => r.key === target.key && r.id.review);
        if (fresh?.eligible[kind]) cx.spawn(async (cx) => this.request(kind, target.key, cx));
        else this.message = `${target.key} mudou de estado; pedido cancelado`;
      } else if (k === "n" || k === "escape") {
        this.confirm = null;
        this.message = "cancelado";
      } else return;
      cx.stop_propagation();
      cx.notify();
      return;
    }

    this.message = null;
    const i = row ? rows.indexOf(row) : -1;
    if (k === "up" || k === "k") this.select(rows[Math.max(0, i - 1)]);
    else if (k === "down" || k === "j") this.select(rows[Math.min(rows.length - 1, i + 1)]);
    else if (k === "escape" || k === "q") this.close();
    else if (k === "s") cx.spawn(async (cx) => this.request("sync", null, cx));
    else if (!row) return;
    else if (!row.id.review) {
      if (k === "enter" || k === "o") exec("open", [row.url]);
      else if (["l", "R", "a", "r"].includes(e.key_char ?? k)) this.message = "só para PRs de review";
      else return;
    } else if (k === "enter") {
      if (row.eligible.focus) cx.spawn(async (cx) => this.focusPr(row, cx));
      else this.message = "este PR está sendo criado ou removido";
    } else if (k === "o") exec("open", [row.url]);
    else if (k === "l" && !e.modifiers.shift) {
      if (row.eligible.log) this.showLog(row);
    } else if (e.key_char === "R") {
      if (row.eligible.retry) cx.spawn(async (cx) => this.request("retry", row.key, cx));
      else this.message = NOT_ELIGIBLE.retry;
    } else if (e.key_char === "a" || e.key_char === "r") {
      const kind = e.key_char === "a" ? "adopt" : "release";
      if (row.eligible[kind]) this.confirm = { kind, row };
      else this.message = NOT_ELIGIBLE[kind];
    } else return;
    cx.stop_propagation();
    cx.notify();
  }

  columns() {
    const wide = this.width >= 900;
    const roomy = this.width >= 1100;
    return { label: 70, icon: 16, status: 170, ci: 110, started: 52, author: wide ? 120 : 0, stack: roomy ? 104 : 0 };
  }

  line(cells, c, t, opts = {}) {
    const cell = (w, text, color) => div().w(w).flex_none().truncate().text_color(color ?? t.foreground).child(text ?? "");
    return h_flex().h(26).px(10).gap(10).items_center().flex_none()
      .child(cell(c.label, cells.label, opts.dim ? t.dim : t.muted))
      .child(cell(c.icon, cells.icon, opts.iconColor ?? t.muted))
      .child(cell(c.status, cells.status, opts.dim ? t.dim : t.foreground))
      .child(cell(c.ci, cells.ci, opts.ciColor ?? t.muted))
      .child(cell(c.started, cells.started, t.muted))
      .when(c.author > 0, (el) => el.child(cell(c.author, cells.author, t.muted)))
      .when(c.stack > 0, (el) => el.child(cell(c.stack, cells.stack, t.muted)))
      .child(div().flex_1().min_w(0).truncate().text_color(opts.dim ? t.dim : t.foreground).child(cells.title ?? ""));
  }

  rowColors(row, t) {
    const icon = { "●": t.done, "◐": t.working, "✗": t.blocked, "⊘": t.blocked, "○": t.dim, "◆": t.accent, "✓": t.done }[row.icon] ?? t.muted;
    const ci = row.ci.startsWith("✗") ? t.blocked : row.ci.startsWith("✓") ? t.done : row.ci.startsWith("◐") ? t.working : t.muted;
    return { iconColor: icon, ciColor: ci };
  }

  list(t) {
    const c = this.columns();
    const sel = this.selectedRow();
    const head = this.line({ label: "PR", icon: "", status: "Estado", ci: "CI", started: "Início", author: "Autor", stack: "Empilhado", title: "Título" }, c, t, { dim: true });
    const items = [head];
    let section = null;
    for (const row of this.rows()) {
      if (row.section !== section) {
        section = row.section;
        items.push(div().px(10).pt(10).pb(2).text_size(12).font_semibold().text_color(t.foreground).child(SECTION[section] ?? section));
      }
      const isSel = row === sel;
      items.push(
        div().id(`row-${row.section}-${row.key}`).rounded(6)
          .when(isSel, (el) => el.bg(t.selected))
          .hover((s) => s.bg(isSel ? t.selected : t.hover))
          .on_click((_e, cx) => { this.select(row); this.message = null; cx.notify(); })
          .child(this.line(row, c, t, this.rowColors(row, t))),
      );
    }
    return v_flex().flex_1().min_h(0).overflow_y_scrollbar().children(items);
  }

  counter(title, value, t) {
    return v_flex().px(12).py(6).gap(2).rounded(8).border_1().border_color(t.border).min_w(110)
      .child(div().text_size(11).text_color(t.dim).child(title))
      .child(div().text_size(18).font_semibold().text_color(t.foreground).child(value));
  }

  bottom(t) {
    const v = this.view;
    const lines = [];
    for (const e of v.header.errors) lines.push(div().text_color(t.blocked).child(`! ${e}`));
    if (v.alerts.length) {
      lines.push(div().font_semibold().text_color(t.foreground).child("Alertas"));
      for (const a of v.alerts) lines.push(div().pl(8).truncate().text_color(t.muted).child(a));
    }
    if (v.empty_reason) lines.push(div().text_color(t.muted).child(v.empty_reason));
    if (v.request_line) lines.push(div().text_color(t.foreground).child(v.request_line));
    return v_flex().gap(2).text_size(12).children(lines);
  }

  panel(_cx) {
    const t = theme();
    const v = this.view;
    const bad = v && (v.header.stale || v.header.errors.length > 0);
    const footer = this.confirm
      ? div().text_color(t.blocked).font_semibold().child(confirmText(this.confirm.kind, this.confirm.row.key))
      : div().text_color(this.message ? t.foreground : t.dim).child(this.message ?? FOOTER);
    const root = v_flex().id("root").track_focus(this.handle).size_full().p(16).gap(12).bg(t.popover).text_size(13)
      .on_size_change((size, cx) => { this.width = size.width; cx.notify(); })
      .on_key_down((e, cx) => this.onKey(e, cx))
      .child(h_flex().gap(12).items_center()
        .child(div().text_size(15).font_semibold().text_color(t.foreground).child("reviewq"))
        .child(div().text_color(bad ? t.blocked : t.dim).child(v?.header.sync ?? ""))
        .child(div().flex_1())
        .child(div().text_size(11).text_color(t.dim).child("esc")));
    if (!v) {
      return root.child(div().text_color(this.error ? t.blocked : t.dim).child(this.error ?? "carregando…"));
    }
    return root
      .child(h_flex().gap(10).items_end()
        .child(this.counter("Pendentes", v.pending, t))
        .child(this.counter("Feitas hoje", v.today, t))
        .child(this.counter("Abertos 24h", v.opened_24h, t))
        .child(div().pb(6).text_size(12).text_color(t.dim).child(v.today_detail)))
      .child(this.list(t))
      .when(Boolean(this.error), (el) => el.child(div().text_size(12).text_color(t.blocked).child(this.error)))
      .child(this.bottom(t))
      .child(div().text_size(12).child(footer));
  }
}
