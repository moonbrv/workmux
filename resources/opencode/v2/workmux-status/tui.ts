import { spawn } from 'node:child_process';
import { Plugin } from '@opencode/plugin/tui';
import { StatusTracker, type WindowStatus } from './status';

function hasLocalPane(): boolean {
  // Match workmux's backend selection, but require an identifiable local pane.
  const panes: Record<string, boolean> = {
    tmux: !!process.env.TMUX && !!process.env.TMUX_PANE,
    wezterm: !!process.env.WEZTERM_PANE,
    zellij: !!process.env.ZELLIJ_PANE_ID,
    kitty: !!process.env.KITTY_WINDOW_ID,
  };
  const override = process.env.WORKMUX_BACKEND;
  if (override) return panes[override] ?? false;
  if (process.env.TMUX !== undefined) return panes.tmux;
  if (process.env.WEZTERM_PANE) return panes.wezterm;
  if (process.env.ZELLIJ || process.env.ZELLIJ_PANE_ID || process.env.ZELLIJ_SESSION_NAME) return panes.zellij;
  return panes.kitty;
}

function workmux(...args: string[]): Promise<boolean> {
  return new Promise((resolve) => {
    // Workmux reads stdin for hook metadata. An open pipe makes it wait forever.
    const child = spawn('workmux', args, { stdio: 'ignore', timeout: 3000 });
    let spawnError: string | undefined;
    child.on('error', (error: NodeJS.ErrnoException) => {
      spawnError = error.code ?? 'spawn error';
    });
    child.on('close', (code, signal) => {
      if (code === 0 && !spawnError) return resolve(true);
      console.warn(`[workmux.status] workmux ${args.join(' ')} failed (${spawnError ?? signal ?? `exit ${code}`})`);
      resolve(false);
    });
  });
}

export default Plugin.define({
  id: 'workmux.status',
  setup(ctx) {
    // The shared server has no reliable relationship to this terminal pane.
    if (!hasLocalPane()) return;

    let closed = false;
    let desired: WindowStatus | undefined;
    let reported: WindowStatus | undefined;
    let queue = workmux('register-agent').then(() => {});

    const tracker = new StatusTracker((status) => {
      desired = status;
      queue = queue.then(async () => {
        if (closed || desired === reported || !desired) return;
        const next = desired;
        if (await workmux('set-window-status', next)) reported = next;
      });
    });

    // The event stream belongs to the shared service. A location alone cannot
    // distinguish two OpenCode windows open in the same worktree.
    const roots = new Set<string>();
    const owned = new Set<string>();
    const parents = new Map<string, string>();
    const deleted = new Set<string>();

    function isOwned(sessionID: string): boolean {
      if (deleted.has(sessionID)) return false;
      if (owned.has(sessionID)) return true;
      const root = ctx.data.session.root(sessionID);
      const parent = parents.get(sessionID) ?? ctx.data.session.get(sessionID)?.parentID;
      if (!roots.has(root) && !(parent && isOwned(parent))) return false;
      owned.add(sessionID);
      return true;
    }

    async function hydrate(root: string) {
      const family = ctx.data.session.family(root);
      for (const sessionID of family) {
        if (!isOwned(sessionID)) continue;
        await Promise.allSettled([
          ctx.data.session.sync(sessionID),
          ctx.data.session.permission.sync(sessionID),
          ctx.data.session.form.sync(sessionID),
        ]);
        if (closed || !isOwned(sessionID)) continue;
        const pending = [
          ...(ctx.data.session.permission.list(sessionID) ?? []).map((request) => `permission:${request.id}`),
          ...(ctx.data.session.form.list(sessionID) ?? []).map((form) => `form:${form.id}`),
        ];
        if (ctx.data.session.status(sessionID) === 'running' || pending.length) {
          tracker.seed(sessionID, pending);
        }
      }
    }

    function claim(sessionID: string) {
      if (deleted.has(sessionID)) return;
      const root = ctx.data.session.root(sessionID);
      owned.add(sessionID);
      if (roots.has(root)) return;
      roots.add(root);
      void hydrate(root);
    }

    function captureLocalSessions() {
      const route = ctx.ui.router.current();
      if (route.type === 'session') claim(route.sessionID);
      for (const tab of ctx.ui.tabs.list()) claim(tab.sessionID);
    }

    const stop = ctx.data.listen(({ details: event }) => {
      if (closed) return;
      captureLocalSessions();

      if (event.type === 'session.created') {
        deleted.delete(event.data.sessionID);
        if (event.data.parentID) {
          parents.set(event.data.sessionID, event.data.parentID);
          if (isOwned(event.data.parentID)) owned.add(event.data.sessionID);
        }
        return;
      }

      const sessionID = event.type === 'form.created'
        ? event.data.form.sessionID
        : 'sessionID' in event.data && typeof event.data.sessionID === 'string'
          ? event.data.sessionID
          : undefined;
      if (!sessionID || !isOwned(sessionID)) return;

      switch (event.type) {
        case 'session.execution.started':
          tracker.start(sessionID);
          break;
        case 'session.status':
          if (event.data.status.type === 'busy' || event.data.status.type === 'retry') tracker.busy(sessionID);
          if (event.data.status.type === 'idle') tracker.finish(sessionID);
          break;
        case 'permission.asked':
          tracker.wait(sessionID, `permission:${event.data.id}`);
          break;
        case 'permission.replied':
          tracker.resume(sessionID, `permission:${event.data.requestID}`);
          break;
        case 'form.created':
          tracker.wait(sessionID, `form:${event.data.form.id}`);
          break;
        case 'form.replied':
        case 'form.cancelled':
          tracker.resume(sessionID, `form:${event.data.id}`);
          break;
        case 'session.execution.succeeded':
        case 'session.execution.failed':
        case 'session.execution.interrupted':
        case 'session.idle':
          tracker.finish(sessionID);
          break;
        case 'session.deleted':
          tracker.forget(sessionID);
          owned.delete(sessionID);
          parents.delete(sessionID);
          roots.delete(sessionID);
          deleted.add(sessionID);
          break;
      }
    });

    captureLocalSessions();
    // A newly opened session can become visible after its first server event.
    // Reconcile client-local navigation so its already-running turn is picked up.
    const timer = setInterval(captureLocalSessions, 500);
    return () => {
      closed = true;
      clearInterval(timer);
      stop();
    };
  },
});
