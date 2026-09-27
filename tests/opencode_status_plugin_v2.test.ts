import { afterEach, beforeEach, expect, mock, test } from 'bun:test';
import { chmod, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

mock.module('@opencode/plugin/tui', () => ({ Plugin: { define: (definition: unknown) => definition } }));

const { default: plugin } = await import('../resources/opencode/v2/workmux-status/tui');
const originalTmux = process.env.TMUX;
const originalPane = process.env.TMUX_PANE;
const originalPath = process.env.PATH;
const originalLog = process.env.WORKMUX_TEST_LOG;
let testDir: string;

beforeEach(async () => {
  testDir = await mkdtemp(join(tmpdir(), 'workmux-status-test-'));
  const binary = join(testDir, 'workmux');
  // The real workmux reads stdin to check for hook input. This stub does too:
  // leaving execFile's stdin pipe open prevents it from registering at all.
  await writeFile(binary, '#!/bin/sh\n/bin/cat >/dev/null\nprintf "%s|%s\\n" "$TMUX_PANE" "$*" >> "$WORKMUX_TEST_LOG"\n');
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
  if (originalTmux === undefined) delete process.env.TMUX;
  else process.env.TMUX = originalTmux;
  if (originalPane === undefined) delete process.env.TMUX_PANE;
  else process.env.TMUX_PANE = originalPane;
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

test('does nothing when the client is outside tmux', async () => {
  delete process.env.TMUX;
  delete process.env.TMUX_PANE;
  await plugin.setup({} as any);
  expect(await calls()).toEqual([]);
});
