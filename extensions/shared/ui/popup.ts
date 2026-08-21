/** Toolbar popup: status at a glance, and the three controls used most. */

import { api } from '../src/browser.js';
import type { Message, Response, StatusReport } from '../src/messaging.js';

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (node === null) throw new Error(`missing element #${id}`);
  return node as T;
}

async function send(message: Message): Promise<Response> {
  return (await api.runtime.sendMessage(message)) as Response;
}

function render(status: StatusReport): void {
  const pill = el('status-pill');
  const active = status.enabled && !status.paused;
  pill.textContent = !status.engineReady
    ? 'Not running'
    : status.paused
      ? 'Paused'
      : status.enabled
        ? 'On'
        : 'Off';
  pill.className = `pill ${active && status.engineReady ? 'pill--on' : 'pill--off'}`;

  el<HTMLInputElement>('enabled').checked = status.enabled;

  el('site-host').textContent = status.host ?? 'No site in this tab';
  const allow = el<HTMLInputElement>('allow-site');
  allow.checked = status.hostAllowlisted;
  allow.disabled = status.host === null;

  el('blocked-tab').textContent = String(status.blockedOnTab);
  el('blocked-total').textContent = String(status.blockedTotal);
  el('rules-loaded').textContent = status.rulesLoaded.toLocaleString();

  const resume = el<HTMLButtonElement>('resume');
  resume.hidden = !status.paused;

  const error = el('engine-error');
  if (status.engineError !== null) {
    error.hidden = false;
    error.textContent = `Filtering engine did not start: ${status.engineError}`;
  } else {
    error.hidden = true;
  }
}

let currentHost: string | null = null;

async function refresh(): Promise<void> {
  const response = await send({ type: 'getStatus' });
  if (!response.ok || !('status' in response)) return;
  currentHost = response.status.host;
  render(response.status);

  // The blocked counter only means anything when counting is switched on.
  const statsOff = el('stats-off');
  const diagnostics = await send({ type: 'getDiagnostics' });
  if (diagnostics.ok && 'diagnostics' in diagnostics) {
    const stats = diagnostics.diagnostics.statistics as { enabled: boolean } | undefined;
    statsOff.hidden = stats?.enabled !== false;
  }
}

function wire(): void {
  el<HTMLInputElement>('enabled').addEventListener('change', async (event) => {
    const enabled = (event.target as HTMLInputElement).checked;
    await send({ type: 'setEnabled', enabled });
    await refresh();
  });

  el<HTMLInputElement>('allow-site').addEventListener('change', async (event) => {
    if (currentHost === null) return;
    const allow = (event.target as HTMLInputElement).checked;
    await send(
      allow
        ? { type: 'allowlistAdd', domain: currentHost }
        : { type: 'allowlistRemove', domain: currentHost },
    );
    await refresh();
    // The page needs a reload for the change to take effect on what is on
    // screen; do it for the user rather than leaving them to wonder.
    const tabs = await api.tabs.query({ active: true, currentWindow: true });
    if (tabs[0]?.id !== undefined) await api.tabs.reload(tabs[0].id);
  });

  for (const button of document.querySelectorAll<HTMLButtonElement>('[data-pause]')) {
    button.addEventListener('click', async () => {
      await send({ type: 'pause', durationSeconds: Number(button.dataset.pause) });
      await refresh();
    });
  }

  el('resume').addEventListener('click', async () => {
    await send({ type: 'resume' });
    await refresh();
  });

  el('open-settings').addEventListener('click', () => {
    void api.runtime.openOptionsPage();
    window.close();
  });
}

wire();
void refresh();
