/** Full settings page: subscriptions, allowlist, custom rules, diagnostics. */

import { api } from '../src/browser.js';
import type { Message, Response } from '../src/messaging.js';
import { normalizeDomain, type Settings } from '../src/settings.js';

function el<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (node === null) throw new Error(`missing element #${id}`);
  return node as T;
}

async function send(message: Message): Promise<Response> {
  return (await api.runtime.sendMessage(message)) as Response;
}

let settings: Settings;

function renderSubscriptions(): void {
  const list = el('subscriptions');
  list.replaceChildren(
    ...settings.subscriptions.map((sub) => {
      const li = document.createElement('li');
      const label = document.createElement('label');
      label.className = 'switch switch--compact';
      const input = document.createElement('input');
      input.type = 'checkbox';
      input.checked = sub.enabled;
      input.addEventListener('change', () => {
        sub.enabled = input.checked;
      });
      const track = document.createElement('span');
      track.className = 'switch__track';
      track.setAttribute('aria-hidden', 'true');
      const text = document.createElement('span');
      text.className = 'switch__label';
      text.textContent = sub.title;
      label.append(input, track, text);

      const meta = document.createElement('span');
      meta.className = 'meta';
      meta.textContent = sub.ruleCount === undefined ? 'bundled' : `${sub.ruleCount} rules`;

      li.append(label, meta);
      return li;
    }),
  );
}

function renderAllowlist(): void {
  const list = el('allowlist');
  if (settings.allowlist.length === 0) {
    const li = document.createElement('li');
    li.className = 'meta';
    li.textContent = 'No sites allowed yet.';
    list.replaceChildren(li);
    return;
  }
  list.replaceChildren(
    ...settings.allowlist.map((domain) => {
      const li = document.createElement('li');
      const name = document.createElement('span');
      name.textContent = domain;
      const remove = document.createElement('button');
      remove.type = 'button';
      remove.className = 'link';
      remove.textContent = 'Remove';
      remove.addEventListener('click', async () => {
        await send({ type: 'allowlistRemove', domain });
        await load();
      });
      li.append(name, remove);
      return li;
    }),
  );
}

async function renderDiagnostics(): Promise<void> {
  const response = await send({ type: 'getDiagnostics' });
  el('diagnostics').textContent =
    response.ok && 'diagnostics' in response
      ? JSON.stringify(response.diagnostics, null, 2)
      : 'Diagnostics unavailable.';
}

async function load(): Promise<void> {
  const response = await send({ type: 'getSettings' });
  if (!response.ok || !('settings' in response)) return;
  settings = response.settings;

  renderSubscriptions();
  renderAllowlist();
  el<HTMLTextAreaElement>('custom-rules').value = settings.customRules;
  el<HTMLInputElement>('statistics').checked = settings.privacy.statisticsEnabled;
  await renderDiagnostics();
}

function wire(): void {
  el('allow-add').addEventListener('click', async () => {
    const input = el<HTMLInputElement>('allow-input');
    const error = el('allow-error');
    const domain = normalizeDomain(input.value);
    if (domain === null) {
      error.hidden = false;
      error.textContent = `"${input.value}" is not a domain.`;
      return;
    }
    error.hidden = true;
    input.value = '';
    await send({ type: 'allowlistAdd', domain });
    await load();
  });

  el('reset-stats').addEventListener('click', async () => {
    await send({ type: 'resetStatistics' });
    await renderDiagnostics();
  });

  el('save').addEventListener('click', async () => {
    settings.customRules = el<HTMLTextAreaElement>('custom-rules').value;
    settings.privacy.statisticsEnabled = el<HTMLInputElement>('statistics').checked;
    const response = await send({ type: 'saveSettings', settings });
    const saved = el('saved');
    saved.hidden = false;
    saved.textContent = response.ok ? 'Saved.' : `Could not save: ${(response as { error: string }).error}`;
    saved.className = response.ok ? 'notice notice--ok' : 'notice notice--error';
    setTimeout(() => {
      saved.hidden = true;
    }, 2500);
    await load();
  });
}

wire();
void load();
