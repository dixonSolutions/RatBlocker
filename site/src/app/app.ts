import { Component, effect, signal } from '@angular/core';
import { RouterLink, RouterLinkActive, RouterOutlet } from '@angular/router';
import { Button } from '@openng/optimus-ui/button';

import { PROJECT } from './data/project';

@Component({
  selector: 'app-root',
  imports: [RouterOutlet, RouterLink, RouterLinkActive, Button],
  templateUrl: './app.html',
  styleUrl: './app.scss',
})
export class App {
  readonly project = PROJECT;
  readonly year = new Date().getFullYear();

  /**
   * Dark mode.
   *
   * Starts from the system preference and can be overridden by the toggle.
   * Guarded for the server pass: prerendering runs in Node, where there is no
   * `matchMedia` and no `document`.
   */
  readonly dark = signal(
    typeof window !== 'undefined' &&
      window.matchMedia?.('(prefers-color-scheme: dark)').matches === true,
  );

  readonly nav = [
    { label: 'How it works', path: '/how-it-works' },
    { label: 'Install', path: '/install' },
    { label: 'Releases', path: '/releases' },
  ];

  constructor() {
    effect(() => {
      const dark = this.dark();
      if (typeof document !== 'undefined') {
        document.documentElement.classList.toggle('dark', dark);
      }
    });
  }

  toggleTheme(): void {
    this.dark.update((value) => !value);
  }
}
