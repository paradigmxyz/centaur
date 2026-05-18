import { defineConfig } from 'vocs'

import { sidebar } from './sidebar.js'

const basePath = process.env.VOCS_BASE_PATH || undefined

export default defineConfig({
  rootDir: '.',
  title: 'Centaur',
  titleTemplate: '%s - Centaur',
  description: 'The production control plane for shared AI agents, tools, workflows, and sandboxes.',
  iconUrl: '/centaur.png',
  logoUrl: '/centaur.png',
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
      text: 'What is Centaur?',
      link: '/what-is-centaur',
      match: (path) => path === '/what-is-centaur',
    },
    {
      text: 'Quickstart',
      link: '/quickstart',
      match: (path) => path === '/quickstart',
    },
    {
      text: 'Deploying in Production',
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
