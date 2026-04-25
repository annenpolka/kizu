use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use crossterm::event::{Event, EventStream};
use futures_util::StreamExt;
use tokio::time::{MissedTickBehavior, interval, sleep};

use crate::watcher::{self, WatchEvent};

use super::{App, EditorInvocation, KeyEffect, ViewMode};

/// Async event loop. See ADR-0003 / ADR-0005.
pub async fn run() -> Result<()> {
    use std::io::Write;
    let log_path = std::env::var("KIZU_STARTUP_TIMING_FILE").ok();
    let stage = |label: &str, t: Instant| {
        if let Some(path) = log_path.as_deref()
            && let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
        {
            let _ = writeln!(f, "[kizu-startup] {label:<28} +{:?}", t.elapsed());
        }
    };
    let t_total = Instant::now();
    let cwd = std::env::current_dir().context("reading current directory")?;
    stage("current_dir", t_total);
    let mut terminal = ratatui::try_init().context("initializing terminal")?;
    // Enable bracketed paste so terminals send IME-committed text
    // (e.g. Japanese kanji) as Event::Paste instead of individual
    // keystrokes. Without this, IME composition is invisible in
    // raw mode and committed text may arrive garbled.
    {
        use crossterm::ExecutableCommand;
        let _ = std::io::stdout().execute(crossterm::event::EnableBracketedPaste);
    }
    stage("ratatui::try_init", t_total);
    let result = async {
        // Show something immediately, even before the initial bootstrap
        // `git diff` completes. On large repos this avoids a black screen
        // during the synchronous bootstrap shell-outs.
        terminal
            .draw(|frame| {
                let area = frame.area();
                frame.render_widget(
                    ratatui::widgets::Paragraph::new("Loading kizu...")
                        .alignment(ratatui::layout::Alignment::Center),
                    area,
                );
            })
            .context("ratatui loading draw")?;
        stage("draw Loading...", t_total);

        let t_bootstrap = Instant::now();
        let mut app = App::bootstrap(cwd)?;
        stage("App::bootstrap", t_bootstrap);

        // Write session file so the Stop hook can scope its scan
        // to files changed since this baseline. Best-effort: a
        // failure here is not fatal to the TUI itself.
        if let Err(e) = crate::session::write_session(&app.root, &app.baseline_sha) {
            eprintln!("warning: failed to write kizu session file: {e}");
        }

        // Session isolation: older events are now filtered on ingest
        // via `session_start_ms` rather than by bulk-deleting the
        // shared events directory. The delete path used to destroy a
        // concurrently-running kizu session's live history on the
        // same project; the filter approach is non-destructive.
        // Diff snapshots were already seeded inside `App::bootstrap`
        // from the same `git diff` that produced the initial file
        // list, so the first stream event shows only the per-op
        // delta without an extra startup subprocess sweep.

        // Draw one static frame before watcher startup. On macOS the
        // PollWatcher fallback may take noticeable time to arm because it
        // performs an initial scan; showing the bootstrap snapshot first
        // keeps startup feeling immediate instead of blank-screening until
        // watcher init finishes.
        terminal
            .draw(|frame| crate::ui::render(frame, &app))
            .context("ratatui initial draw")?;
        stage("draw bootstrap snapshot", t_total);

        let t_watcher = Instant::now();
        let mut watch = watcher::start(
            &app.root,
            &app.git_dir,
            &app.common_git_dir,
            app.current_branch_ref.as_deref(),
        )?;
        stage("watcher::start", t_watcher);

        // Replay any event files that were written in the gap
        // between `clean_stale_events` and `watcher::start`. Without
        // this the next event for the same file would absorb the
        // dropped operation's contents into its op_diff because the
        // seeded snapshot is still the pre-edit state. Dedup inside
        // `handle_event_log` makes this safe even when the watcher
        // later re-delivers the same file.
        if let Some(events_dir) = crate::paths::events_dir(&app.root) {
            app.replay_events_dir(&events_dir);
        }
        stage("replay startup-gap events", t_total);
        let result = run_loop(&mut terminal, &mut app, &mut watch).await;
        crate::session::remove_session(&app.root);
        result
    }
    .await;
    {
        use crossterm::ExecutableCommand;
        let _ = std::io::stdout().execute(crossterm::event::DisableBracketedPaste);
    }
    let _ = ratatui::try_restore();
    result
}

async fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    watch: &mut watcher::WatchHandle,
) -> Result<()> {
    let mut events = EventStream::new();

    // ~60 fps frame tick. Only polled inside `select!` when an animation
    // is live — idle frames never pay the cost. `Skip` means a long
    // idle gap doesn't turn into a burst of catch-up ticks once the
    // user kicks off a new animation.
    let mut frame = interval(Duration::from_millis(16));
    frame.set_missed_tick_behavior(MissedTickBehavior::Skip);

    // notify backends can have a short arm-up window right after startup.
    // Without a one-shot self-heal refresh, an edit that lands during that
    // gap can be missed forever until the *next* filesystem event. The
    // existing watcher tests used `sleep(150ms)` to paper over this; the app
    // should instead recover on its own.
    let startup_refresh = sleep(Duration::from_millis(400));
    tokio::pin!(startup_refresh);
    let mut startup_refresh_pending = true;

    while !app.should_quit {
        // Draw at the top of the loop so the bootstrap state is visible
        // before we ever block on `select!`.
        terminal
            .draw(|frame| crate::ui::render(frame, app))
            .context("ratatui draw")?;

        // Retire finished animations after the frame that showed their
        // final position — the next frame will then draw the static
        // target without another tween sample.
        app.tick_anim(Instant::now());
        app.tick_file_view_anim();

        tokio::select! {
            event = events.next() => {
                match event {
                    Some(Ok(Event::Key(key))) => {
                        app.input_health = None;
                        let effect = app.handle_key(key);
                        match effect {
                            KeyEffect::OpenEditor(inv) => {
                                if let Err(err) = run_external_editor(terminal, inv) {
                                    app.last_error = Some(format!("editor: {err:#}"));
                                }
                            }
                            other => apply_key_effect(other, app, watch),
                        }
                    }
                    Some(Ok(Event::Paste(text))) => {
                        app.input_health = None;
                        app.handle_paste(&text);
                    }
                    Some(Ok(_)) => {
                        app.input_health = None;
                    }
                    Some(Err(e)) => {
                        app.input_health = Some(format!("input: {e}"));
                    }
                    None => {
                        app.input_health = Some("input: event stream ended".into());
                        app.should_quit = true;
                    }
                }
            }
            watch_event = watch.events.recv() => {
                if let Some(first) = watch_event {
                    startup_refresh_pending = false;
                    // Drain any events that piled up behind `first` and
                    // hand the whole burst to `handle_watch_burst` so the
                    // coalescing + health-transition rules stay testable
                    // in one place.
                    let mut burst: Vec<WatchEvent> = vec![first];
                    while let Ok(more) = watch.events.try_recv() {
                        burst.push(more);
                    }
                    let (need_recompute, need_head_dirty) = app.handle_watch_burst(burst);
                    if need_recompute {
                        watch.refresh_worktree_watches();
                        // In stream mode, don't overwrite files/layout with
                        // git diff — the scroll view shows stream events.
                        // The diff will be refreshed when the user tabs back.
                        if app.view_mode != ViewMode::Stream {
                            app.recompute_diff();
                        }
                    }
                    if need_head_dirty {
                        app.mark_head_dirty();
                    }
                } else {
                    app.last_error = Some("watcher: event channel closed".into());
                    app.should_quit = true;
                }
            }
            _ = &mut startup_refresh, if startup_refresh_pending => {
                startup_refresh_pending = false;
                if app.view_mode != ViewMode::Stream {
                    app.recompute_diff();
                }
            }
            _ = frame.tick(), if app.anim.is_some() || app.file_view.as_ref().is_some_and(|fv| fv.anim.is_some()) => {
                // The tick itself carries no payload — falling through
                // the bottom of the select! loops back to the `draw`
                // call at the top, which is the whole point.
            }
        }
    }

    Ok(())
}

/// Dispatch post-key-handler side effects back onto the watcher.
/// Factored out so `run_loop` stays focused on the event-loop
/// plumbing and tests can reason about the effect contract without
/// spinning up a real terminal.
fn apply_key_effect(effect: KeyEffect, app: &App, watch: &watcher::WatchHandle) {
    match effect {
        KeyEffect::None => {}
        KeyEffect::ReconfigureWatcher => {
            watch.update_current_branch_ref(app.current_branch_ref.as_deref());
        }
        KeyEffect::OpenEditor(_) => {
            // Handled inline inside `run_loop`: the editor
            // spawn needs mutable access to the terminal for the
            // suspend/resume dance, which this `&App` /
            // `&WatchHandle` helper cannot provide. Any
            // `OpenEditor` that reaches this arm is a caller
            // bug.
            debug_assert!(
                false,
                "OpenEditor must be handled by run_loop, not apply_key_effect"
            );
        }
    }
}

/// Suspend the ratatui terminal, run an external editor
/// synchronously, then re-enter the alternate screen and force a
/// full repaint on the next draw tick. Blocks the event loop for
/// the editor's lifetime — intentional, because the user is
/// inside the editor anyway and no diff-view update would be
/// visible under it.
fn run_external_editor(
    terminal: &mut ratatui::DefaultTerminal,
    invocation: EditorInvocation,
) -> Result<()> {
    use crossterm::{
        ExecutableCommand,
        event::{DisableBracketedPaste, EnableBracketedPaste},
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    };
    use std::io::stdout;

    // Tear down the ratatui terminal state so the child editor
    // sees a plain cooked terminal. Errors here are surfaced —
    // half-suspended state is worse than not launching the editor
    // at all.
    disable_raw_mode().context("disable raw mode before editor")?;
    let mut out = stdout();
    out.execute(LeaveAlternateScreen)
        .context("leave alternate screen before editor")?;
    out.execute(DisableBracketedPaste).ok();

    let status = std::process::Command::new(&invocation.program)
        .args(&invocation.args)
        .status()
        .with_context(|| format!("spawning editor `{}`", invocation.program));

    // Always re-arm the alternate screen + raw mode even if the
    // spawn itself failed. Otherwise a mistyped `$EDITOR` would
    // leave the user stranded at a raw-mode prompt.
    enable_raw_mode().context("re-enable raw mode after editor")?;
    stdout()
        .execute(EnterAlternateScreen)
        .context("re-enter alternate screen after editor")?;
    stdout().execute(EnableBracketedPaste).ok();
    terminal.clear().ok();

    let status = status?;
    if !status.success() {
        return Err(anyhow!(
            "editor `{}` exited with status {}",
            invocation.program,
            status
        ));
    }
    Ok(())
}
