import { Routes } from '@angular/router';

/**
 * Every route is lazily loaded and prerendered. Keeping them lazy means each
 * page ships only what it uses, which matters because the Optimus components
 * differ a lot from page to page.
 */
export const routes: Routes = [
  {
    path: '',
    loadComponent: () => import('./pages/home/home').then((m) => m.Home),
    title: 'RatBlocker — local, private ad and tracker blocking',
  },
  {
    path: 'how-it-works',
    loadComponent: () => import('./pages/how-it-works/how-it-works').then((m) => m.HowItWorks),
    title: 'How it works — RatBlocker',
  },
  {
    path: 'install',
    loadComponent: () => import('./pages/install/install').then((m) => m.Install),
    title: 'Install — RatBlocker',
  },
  {
    path: 'releases',
    loadComponent: () => import('./pages/releases/releases').then((m) => m.Releases),
    title: 'Releases — RatBlocker',
  },
  { path: '**', redirectTo: '' },
];
