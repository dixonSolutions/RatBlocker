import { RenderMode, ServerRoute } from '@angular/ssr';

/**
 * Every route is prerendered. GitHub Pages serves static files only, so there
 * is nothing at request time to render on.
 */
export const serverRoutes: ServerRoute[] = [
  {
    path: '**',
    renderMode: RenderMode.Prerender,
  },
];
