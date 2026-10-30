# reviewq

Your GitHub review queue inside Herdr: a status bar block with how many reviews are pending and ready, and a panel (a dialog over the window) that lists every review and your own open PRs, with the worktree state, CI, when you started and whether the PR is stacked. Enter takes you to the PR's space; the daemon that prepares those worktrees lives in the same repo.

## What you see

- **The bar**, a block about 260px wide and 28px high, meant for a status bar. Background `chrome`, 12px side padding, 10px gaps, text 11.8px, optional 1px `border` line on top (prop `border: "top"`).
  - `reviewq` in `dim` (in `blocked` when the sync is stale, GitHub reported errors or the binary failed).
  - `N pendentes` in `muted`, `N prontos` in `done` when any worktree is ready, `N hoje` in `dim`.
  - Before the first load: `…`. The tooltip shows the sync age and when "feitas hoje" was counted, or the error.
- **The panel**, a dialog at 85% × 80% of the window, `popover` background, 16px padding, 12px gaps, text 13px.
  - Header: "reviewq" 15px semibold, the sync age (`sync 2s`) in `dim` or `blocked` when stale, and `esc` at the right.
  - Three counters (bordered boxes, 8px radius, title 11px `dim`, value 18px semibold): Pendentes, Feitas hoje, Abertos 24h; beside them "atualizado há 3min" in 12px `dim`.
  - The list, scrolling: a dim column header (PR, Estado, CI, Início, Autor, Empilhado, Título), then section titles in 12px semibold (Iniciados, Pendentes, Meus PRs, Adotados, Sem sync, Saindo) and 26px rows with fixed-width cells: label 70, icon 16, status 170, CI 110, Início 52, Autor 120 (from 900px wide), Empilhado 104 (from 1100px), title takes the rest, every cell truncated.
  - Icon colors: `●`/`✓` `done`, `◐` `working`, `✗`/`⊘` `blocked`, `◆` `accent`, `○` `dim`. CI text: `✗` `blocked`, `✓` `done`, `◐` `working`.
  - Selected row: `selected` background, 6px radius; hover `hover`.
  - Below the list: GitHub errors (`! …` in `blocked`), "Alertas" with the last three alerts, the empty-state reason and the result of the last request ("pedido sync: ok — …").
  - Footer: the key help in `dim`, or a message, or a y/n confirmation in `blocked` semibold.

## How it behaves

- Data comes from `herdr-app-reviewq panel`, which prints the whole view as JSON (rows, counters, header, alerts, request result, and the state folder). The mod never parses `state.json` itself.
- It refreshes when `state.json` or `notifications.jsonl` change in the state folder (`watchFolder`), ignoring the setup logs that change constantly, and every 30 seconds so relative times move.
- The bar shows toasts for new lines in `notifications.jsonl` (`notify`, with `space.select` as the click action when the line has a space) and plays `done` or `blocked` for the daemon's `done` / `request` sounds. The last shown notice is kept in shared `"reviewq.notices.seen"`; on the first run everything already in the file counts as seen.
- Keys in the panel: ↑↓/j k move, enter runs `herdr-app-reviewq focus pr --pr <key>` and closes the dialog (on "Meus PRs", opens the URL), `o` opens the URL, `l` opens the setup log with `less +G` in a new tab, `s` sync, `R` retry, `a` adopt and `r` release after `y`/`n`; esc/q closes. Ineligible actions print the reason in the footer. Before sending a confirmed request, the row is checked again against the latest view.
- A request is sent with `herdr-app-reviewq request <kind> --pr <key> --json`; its id and time go back to `panel --sent-…` so the footer shows "enviado…", then the daemon's verdict.
- Clicking the bar opens the panel; `mod.open:reviewq` opens it from a key (`mod.json` sets the dialog size).

## Built from

- **One mod**, `mods/reviewq/`, `main.js` and `mod.json` (`place: "none"`, `overlay: { kind: "dialog", width: 0.85, height: 0.8 }`).
- **One view, two roles**: with `props().panel` or `props().overlay` it draws the panel (and takes keyboard focus with a focus handle), otherwise the bar. Only the bar delivers toasts.
- **The binary** `~/.local/bin/herdr-app-reviewq` (installed by `service install`), run with `exec`. `panel`, `request`, `focus pr`.
- **Herdr functions**: `exec`, `home`, `readTextFile`, `watchFolder`, `shared`, `setShared`, `props`, `openOverlay`, `closeOverlay`, `notify`, `playSound`, `run` (`tab.new`), `theme`.
- **Programs**: `herdr-app-reviewq`, `open` (macOS), `less`.

## Watch out

- **The mod runs programs and reads files outside its folder**: the reviewq binary and its state folder (`~/.local/state/herdr-app-reviewq`). Through the binary it reaches GitHub (via `gh`) and herdr-core.
- The state folder only becomes known after the first successful load, so a bar that started before the binary was installed starts watching on the next 30-second tick.
- The bar stops the mouse-down so a `window_drag` status bar doesn't swallow the click.
- Shift letters: `R` is matched on `key_char`, `l` only without shift.
- Times and texts come from the binary, in Portuguese and São Paulo time.

## Done when

- `herdr-app check mods/reviewq` passes.
- With the daemon running, the bar shows the same pending count as `herdr-app-reviewq status`.
- `cmd-shift-r` opens the panel with the same rows as `herdr-app-reviewq panel`; arrows move the selection.
- Enter on a ready review shows a space named `#<n> <title>` whose shell is in the worktree; enter again on the same PR reuses it.
- `s` shows "pedido sync: enviado…" and then the daemon's answer.
- A ready worktree produces a toast; clicking it shows the PR's space.
