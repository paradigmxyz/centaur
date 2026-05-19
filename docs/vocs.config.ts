import { createElement, Fragment } from 'react'
import { defineConfig } from 'vocs'

import { sidebar } from './sidebar.js'

const basePath = process.env.VOCS_BASE_PATH || undefined

export default defineConfig({
  rootDir: '.',
  // The dead-link checker doesn't know about static assets shipped via
  // public/ (like our zip and brand SVGs), so downgrade to a warning rather
  // than failing the build.
  checkDeadlinks: 'warn',
  title: 'Centaur',
  titleTemplate: '%s - Centaur',
  description: 'The production control plane for shared AI agents, tools, workflows, and sandboxes.',
  // Browser-tab favicon swaps with system theme via prefers-color-scheme so
  // the icon stays readable on either browser chrome.
  iconUrl: {
    light: '/brand/mark-black.svg',
    dark: '/brand/mark-white.svg',
  },
  // Top-left site logo: black-ink wordmark on light theme, white on dark.
  logoUrl: {
    light: '/brand/lockup-black.svg',
    dark: '/brand/lockup-white.svg',
  },
  // Body copy: Instrument Sans. Code blocks: Geist Mono. Headings use
  // Instrument Serif via a styles.css override since Vocs only natively
  // configures body + mono.
  font: {
    default: { google: 'Instrument Sans' },
    mono: { google: 'Geist Mono' },
  },
  head: createElement(Fragment, null,
    createElement('link', { rel: 'preconnect', href: 'https://fonts.googleapis.com' }),
    createElement('link', { rel: 'preconnect', href: 'https://fonts.gstatic.com', crossOrigin: '' }),
    createElement('link', {
      rel: 'stylesheet',
      href: 'https://fonts.googleapis.com/css2?family=Instrument+Serif:ital@0;1&display=swap',
    }),
    createElement('script', { src: '/centaur-brand-menu.js', defer: true }),
  ),
  ...(basePath ? { basePath } : {}),
  editLink: {
    pattern: 'https://github.com/paradigmxyz/centaur/edit/main/docs/pages/:path',
    text: 'Edit this page',
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
      light: '#ff9318',
      dark: '#ffc517',
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
