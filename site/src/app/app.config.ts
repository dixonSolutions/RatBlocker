import {
  ApplicationConfig,
  provideBrowserGlobalErrorListeners,
  provideZonelessChangeDetection,
} from '@angular/core';
import { provideRouter, withInMemoryScrolling } from '@angular/router';
import { provideOptimus } from '@openng/optimus-ui/config';
import Aura from '@openng/optimus-ui-themes/aura';

import { routes } from './app.routes';

export const appConfig: ApplicationConfig = {
  providers: [
    provideBrowserGlobalErrorListeners(),
    provideZonelessChangeDetection(),
    provideRouter(
      routes,
      // A docs site is read top-to-bottom; landing halfway down a page after
      // following a link is disorienting.
      withInMemoryScrolling({ scrollPositionRestoration: 'enabled', anchorScrolling: 'enabled' }),
    ),
    provideOptimus({
      ripple: true,
      theme: {
        preset: Aura,
        options: {
          // Dark mode follows a class on <html> rather than the media query
          // alone, so the header toggle can override the system preference.
          darkModeSelector: '.dark',
          cssLayer: { name: 'optimus', order: 'theme, base, optimus' },
        },
      },
    }),
  ],
};
