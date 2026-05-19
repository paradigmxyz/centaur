import { createElement, Fragment } from 'react'
import { defineConfig, McpSource } from 'vocs/config'

import { sidebar } from './sidebar.js'

const basePath = process.env.VOCS_BASE_PATH || undefined
const siteUrl = 'https://centaur.run'

function canonicalHref(path: string) {
  if (path === '/') return `${siteUrl}/`
  return `${siteUrl}${path.replace(/\/+$/, '')}/`
}

export default defineConfig({
  rootDir: '.',
  srcDir: '.',
  renderStrategy: 'full-static',
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
  // Top-left site logo: full lockup on docs routes (the sidebar / topNav
  // gets enough space to carry the wordmark). Landing already hides the
  // topNav logo via .vocs_DesktopTopNav_logoWrapper { display: none } so
  // the lockup is only visible on docs pages.
  logoUrl: {
    light: '/brand/lockup-black.svg',
    dark: '/brand/lockup-white.svg',
  },
  mcp: {
    enabled: true,
    sources: [
      McpSource.github({
        name: 'centaur',
        repo: 'paradigmxyz/centaur',
        paths: ['docs', 'services', 'centaur_sdk', 'packages', 'tools', 'workflows'],
      }),
    ],
  },
  // Body copy uses Amp's PolySans via the pages/_root.css override. Docs headings
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
    '/extend/apps': '/og/extend_apps.png',
    '/extend/tools': '/og/extend_tools.png',
    '/extend/workflows': '/og/extend_workflows.png',
    '/extend/skills': '/og/extend_skills.png',
    '/security': '/og/security.png',
    '/secrets/onepassword': '/og/secrets_onepassword.png',
    '/secrets/environment': '/og/secrets_environment.png',
    '/secrets/aws-kms': '/og/secrets_aws-kms.png',
    '/secrets/gcp-secret-manager': '/og/secrets_gcp-secret-manager.png',
    '/secrets/advanced-permissioning': '/og/secrets_advanced-permissioning.png',
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
      text: 'Docs',
      link: '/what-is-centaur',
      match: '/what-is-centaur',
    },
    {
      text: 'GitHub',
      link: 'https://github.com/paradigmxyz/centaur',
    },
  ],
  search: {
    boostDocument(documentId) {
      if (documentId.includes('what-is-centaur')) return 4.5
      if (documentId.includes('quickstart')) return 4
      if (documentId.includes('extend/')) return 3.8
      if (documentId.includes('secrets/')) return 3.8
      if (documentId.includes('security')) return 3.6
      if (documentId.includes('deploying-in-production')) return 3.5
      if (documentId.includes('architecture')) return 3
      return 1
    },
  },
  sidebar,
  theme: {
    accentColor: {
      light: '#00E100',
      dark: '#00E100',
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
