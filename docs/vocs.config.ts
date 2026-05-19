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
  // Browser-tab favicon: rounded-square centaur on a dark grey gradient,
  // self-contained so it reads on both light and dark browser chrome at
  // every favicon size (no separate light/dark variant required).
  iconUrl: '/brand/slack-icon.svg',
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
