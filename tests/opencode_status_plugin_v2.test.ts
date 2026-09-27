import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { chmod, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

mock.module('@opencode/plugin/tui', () => ({ Plugin: { define: (definition: unknown) => definition } }));

const { default: plugin } = await import('../resources/opencode/v2/workmux-status/tui');
const originalPath = process.env.PATH;
const originalLog = process.env.WORKMUX_TEST_LOG;
const muxVariables = ['TMUX', 'TMUX_PANE', 'WEZTERM_PANE', 'ZELLIJ', 'ZELLIJ_PANE_ID', 'ZELLIJ_SESSION_NAME', 'KITTY_WINDOW_ID', 'WORKMUX_BACKEND'] as const;
const originalMux = Object.fromEntries(muxVariables.map((name) => [name, process.env[name]]));
let testDir: string;

beforeEach(async () => {
  for (const name of muxVariables) delete process.env[name];
  testDir = await mkdtemp(join(tmpdir(), 'workmux-status-test-'));
  const binary = join(testDir, 'workmux');
  // The real workmux reads stdin to check for hook input. This stub does too:
  // leaving execFile's stdin pipe open prevents it from registering at all.
  await writeFile(binary, '#!/bin/sh\n/bin/cat >/dev/null\ncase "$WORKMUX_BACKEND" in\n  wezterm) pane="$WEZTERM_PANE";;\n  zellij) pane="$ZELLIJ_PANE_ID";;\n  kitty) pane="$KITTY_WINDOW_ID";;\n  *) pane="${TMUX_PANE:-${WEZTERM_PANE:-${ZELLIJ_PANE_ID:-${KITTY_WINDOW_ID:-}}}}";;\nesac\nprintf "%s|%s\\n" "$pane" "$*" >> "$WORKMUX_TEST_LOG"\n');
  await chmod(binary, 0o755);
  process.env.PATH = `${testDir}:${originalPath}`;
  process.env.WORKMUX_TEST_LOG = join(testDir, 'calls');
});

async function calls() {
  const log = await readFile(process.env.WORKMUX_TEST_LOG!, 'utf8').catch(() => '');
  return log.trim() ? log.trim().split('\n') : [];
}

async function waitForCalls(count: number) {
  for (let i = 0; i < 80; i++) {
    if ((await calls()).length >= count) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  expect(await calls()).toHaveLength(count);
}

afterEach(async () => {
  for (const name of muxVariables) {
    const original = originalMux[name];
    if (original === undefined) delete process.env[name];
    else process.env[name] = original;
  }
  process.env.PATH = originalPath;
  if (originalLog === undefined) delete process.env.WORKMUX_TEST_LOG;
  else process.env.WORKMUX_TEST_LOG = originalLog;
  await rm(testDir, { recursive: true });
});

test('tracks this pane and its children even after closing its tab, but not another pane', async () => {
  process.env.TMUX = 'test-tmux';
  process.env.TMUX_PANE = '%123';
  let handler: (input: any) => void = () => {};
  let tabOpen = true;
  let stopped = false;
  const parents = new Map([['child', 'ours']]);
  const ctx = {
    data: {
      listen(callback: typeof handler) {
        handler = callback;
        return () => { stopped = true; };
      },
      session: {
        root: (id: string) => parents.get(id) ?? id,
        family: (id: string) => id === 'ours' ? ['ours', 'child'] : [id],
        get: (id: string) => parents.has(id) ? { parentID: parents.get(id) } : undefined,
        status: () => 'idle',
        sync: async () => {},
        permission: { sync: async () => {}, list: () => [] },
        form: { sync: async () => {}, list: () => [] },
      },
    },
    ui: {
      router: { current: () => tabOpen ? { type: 'session', sessionID: 'ours' } : { type: 'home' } },
      tabs: { list: () => tabOpen ? [{ sessionID: 'ours' }] : [] },
    },
  };

  const cleanup = await plugin.setup(ctx as any);
  await waitForCalls(1);
  expect(await calls()).toEqual(['%123|register-agent']);
  const emit = (type: string, data: object) => handler({ details: { type, data } });

  emit('session.execution.started', { sessionID: 'other' });
  emit('session.execution.started', { sessionID: 'ours' });
  await waitForCalls(2);
  tabOpen = false;
  emit('session.created', { sessionID: 'child', parentID: 'ours' });
  emit('session.execution.started', { sessionID: 'child' });
  emit('form.created', { form: { id: 'question', sessionID: 'child' } });
  await waitForCalls(3);
  emit('session.execution.succeeded', { sessionID: 'ours' });
  emit('form.replied', { id: 'question', sessionID: 'child' });
  await waitForCalls(4);
  emit('session.execution.succeeded', { sessionID: 'child' });
  await waitForCalls(5);
  emit('session.deleted', { sessionID: 'child' });
  emit('session.status', { sessionID: 'child', status: { type: 'busy' } });

  expect(await calls()).toEqual([
    '%123|register-agent',
    '%123|set-window-status working',
    '%123|set-window-status waiting',
    '%123|set-window-status working',
    '%123|set-window-status done',
  ]);

  emit('session.execution.started', { sessionID: 'ours' });
  await waitForCalls(6);
  emit('permission.asked', { sessionID: 'ours', id: 'allow-edit' });
  await waitForCalls(7);
  emit('permission.replied', { sessionID: 'ours', requestID: 'allow-edit' });
  await waitForCalls(8);
  emit('session.execution.failed', { sessionID: 'ours' });
  await waitForCalls(9);
  expect((await calls()).slice(5)).toEqual([
    '%123|set-window-status working',
    '%123|set-window-status waiting',
    '%123|set-window-status working',
    '%123|set-window-status done',
  ]);
  if (typeof cleanup === 'function') await cleanup();
  expect(stopped).toBe(true);
});

test.each([
  { name: 'WezTerm', vars: { WEZTERM_PANE: '81' }, pane: '81' },
  { name: 'Zellij', vars: { ZELLIJ: '1', ZELLIJ_PANE_ID: '23' }, pane: '23' },
  { name: 'Kitty', vars: { KITTY_WINDOW_ID: '42' }, pane: '42' },
  { name: 'explicit WezTerm backend', vars: { TMUX: 'nested', TMUX_PANE: '%3', WEZTERM_PANE: '81', WORKMUX_BACKEND: 'wezterm' }, pane: '81' },
])('reports status for $name', async ({ vars, pane }) => {
  Object.assign(process.env, vars);
  let handler: (input: any) => void = () => {};
  const ctx = {
    data: {
      listen(callback: typeof handler) { handler = callback; return () => {}; },
      session: {
        root: (id: string) => id,
        family: (id: string) => [id],
        get: () => undefined,
        status: () => 'idle',
        sync: async () => {},
        permission: { sync: async () => {}, list: () => [] },
        form: { sync: async () => {}, list: () => [] },
      },
    },
    ui: {
      router: { current: () => ({ type: 'session', sessionID: 'ours' }) },
      tabs: { list: () => [] },
    },
  };
  const cleanup = await plugin.setup(ctx as any);
  await waitForCalls(1);
  handler({ details: { type: 'session.execution.started', data: { sessionID: 'ours' } } });
  await waitForCalls(2);
  expect(await calls()).toEqual([`${pane}|register-agent`, `${pane}|set-window-status working`]);
  if (typeof cleanup === 'function') await cleanup();
});

test.each([
  { name: 'plain terminal', vars: {} },
  { name: 'tmux without pane ID', vars: { TMUX: 'session' } },
  { name: 'empty tmux marker takes precedence over WezTerm', vars: { TMUX: '', WEZTERM_PANE: '81' } },
  { name: 'Zellij without pane ID', vars: { ZELLIJ: '1' } },
  { name: 'explicit backend without its pane ID', vars: { WEZTERM_PANE: '81', WORKMUX_BACKEND: 'tmux' } },
])('does nothing in $name', async ({ vars }) => {
  Object.assign(process.env, vars);
  await plugin.setup({} as any);
  expect(await calls()).toEqual([]);
});
