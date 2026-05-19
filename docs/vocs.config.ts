import { createElement, Fragment } from 'react'
import { defineConfig } from 'vocs'

import { sidebar } from './sidebar.js'

const basePath = process.env.VOCS_BASE_PATH || undefined
const siteUrl = 'https://centaur.run'

function canonicalHref(path: string) {
  if (path === '/') return `${siteUrl}/`
  return `${siteUrl}${path.replace(/\/+$/, '')}/`
}

export default defineConfig({
  rootDir: '.',
  // The dead-link checker doesn't know about static assets shipped via
  // public/ (like our zip and brand SVGs), so downgrade to a warning rather
  // than failing the build.
  checkDeadlinks: 'warn',
  baseUrl: siteUrl,
  title: 'Centaur',
  titleTemplate: '%s - Centaur',
  description: 'The production control plane for shared AI agents, tools, workflows, and sandboxes.',
  // Browser-tab favicon: standalone centaur mark only (no background frame).
  // Vocs emits a per-scheme <link rel="icon"> pair so the tab shows the
  // black silhouette on light chrome and the white silhouette on dark.
  iconUrl: {
    light: '/brand/mark-black.svg',
    dark: '/brand/mark-white.svg',
  },
  // Top-left site logo: black-ink wordmark on light theme, white on dark.
  logoUrl: {
    light: '/brand/lockup-black.svg',
    dark: '/brand/lockup-white.svg',
  },
  // Body copy uses Amp's PolySans via the styles.css override. Docs headings
  // use Perfectly Nineties, while the landing hero uses Sagittaire Display.
  // Code blocks stay on Geist Mono.
  font: {
    mono: { google: 'Geist Mono' },
  },
  // Open Graph cards are pre-rendered at build time by scripts/build-og.ts
  // (ported from tempoxyz/mpp's /api/og handler) using the vendored brand
  // fonts. Map each known route to its card; new routes fall back to
  // _default.png until the next build picks them up.
  ogImageUrl: {
    '/': '/og/index.png',
    '/what-is-centaur': '/og/what-is-centaur.png',
    '/quickstart': '/og/quickstart.png',
    '/deploying-in-production': '/og/deploying-in-production.png',
    '/architecture': '/og/architecture.png',
    '/brand': '/og/brand.png',
    '/extend/overlay': '/og/extend_overlay.png',
    '/extend/tools': '/og/extend_tools.png',
    '/extend/workflows': '/og/extend_workflows.png',
    '/extend/skills': '/og/extend_skills.png',
    '/secrets/onepassword': '/og/secrets_onepassword.png',
    '/secrets/environment': '/og/secrets_environment.png',
  },
  ...(basePath ? { basePath } : {}),
  editLink: {
    pattern: 'https://github.com/paradigmxyz/centaur/edit/main/docs/pages/:path',
    text: 'Edit this page',
  },
  // Per-page <head>: canonical URL for SEO plus the global font preload and
  // the centaur-brand-menu.js script that powers the right-click logo menu.
  head({ path }) {
    return createElement(Fragment, null,
      createElement('link', { rel: 'canonical', href: canonicalHref(path) }),
      createElement('script', { src: '/centaur-brand-menu.js', defer: true }),
    )
  },
  llms: {
    generateMarkdown: true,
  },
  markdown: {
    code: {
      themes: {
        dark: 'github-dark-default',
        light: 'github-dark-default',
      },
    },
  },
  topNav: [
    {
      text: 'About',
      link: '/what-is-centaur',
      match: (path) => path === '/what-is-centaur',
    },
    {
      text: 'Quickstart',
      link: '/quickstart',
      match: (path) => path === '/quickstart',
    },
    {
      text: 'Deploying',
      link: '/deploying-in-production',
      match: (path) => path === '/deploying-in-production',
    },
    {
      text: 'Architecture',
      link: '/architecture',
      match: (path) => path === '/architecture',
    },
    {
      // GitHub icon link rendered inline at the end of the topNav. The
      // footer's socials entry covers the docs pages; this one keeps the
      // affordance reachable on the landing page where the footer isn't
      // visible above the fold.
      element: createElement(
        'a',
        {
          'aria-label': 'Centaur on GitHub',
          className: 'topnav-github',
          href: 'https://github.com/paradigmxyz/centaur',
          rel: 'noopener noreferrer',
          target: '_blank',
        },
        createElement(
          'svg',
          {
            viewBox: '0 0 98 96',
            xmlns: 'http://www.w3.org/2000/svg',
            'aria-hidden': true,
            width: 18,
            height: 18,
          },
          createElement('path', {
            fill: 'currentColor',
            fillRule: 'evenodd',
            clipRule: 'evenodd',
            d: 'M48.854 0C21.839 0 0 22 0 49.217c0 21.756 13.993 40.172 33.405 46.69 2.427.49 3.316-1.059 3.316-2.362 0-1.141-.08-5.052-.08-9.127-13.59 2.934-16.42-5.867-16.42-5.867-2.184-5.704-5.42-7.17-5.42-7.17-4.448-3.015.324-3.015.324-3.015 4.934.326 7.523 5.052 7.523 5.052 4.367 7.496 11.404 5.378 14.235 4.074.404-3.178 1.699-5.378 3.074-6.6-10.839-1.141-22.243-5.378-22.243-24.283 0-5.378 1.94-9.778 5.014-13.2-.485-1.222-2.184-6.275.486-13.038 0 0 4.125-1.304 13.426 5.052a46.97 46.97 0 0 1 12.214-1.63c4.125 0 8.33.571 12.213 1.63 9.302-6.356 13.427-5.052 13.427-5.052 2.67 6.763.97 11.816.485 13.038 3.155 3.422 5.015 7.822 5.015 13.2 0 18.905-11.404 23.06-22.324 24.283 1.78 1.548 3.316 4.481 3.316 9.126 0 6.6-.08 11.897-.08 13.526 0 1.304.89 2.853 3.316 2.364 19.412-6.52 33.405-24.935 33.405-46.691C97.707 22 75.788 0 48.854 0z',
          }),
        ),
      ),
    },
  ],
  socials: [{ icon: 'github', link: 'https://github.com/paradigmxyz/centaur' }],
  search: {
    boostDocument(documentId) {
      if (documentId.includes('what-is-centaur')) return 4.5
      if (documentId.includes('quickstart')) return 4
      if (documentId.includes('extend/')) return 3.8
      if (documentId.includes('secrets/')) return 3.8
      if (documentId.includes('deploying-in-production')) return 3.5
      if (documentId.includes('architecture')) return 3
      return 1
    },
  },
  sidebar,
  theme: {
    accentColor: {
      light: '#00e100',
      dark: '#00e100',
    },
    colorScheme: 'dark',
    variables: {
      color: {
        background: {
          light: '#ffffff',
          dark: '#050505',
        },
        text: {
          light: '#050505',
          dark: '#f7f7f2',
        },
      },
      content: {
        width: '920px',
      },
    },
  },
})
