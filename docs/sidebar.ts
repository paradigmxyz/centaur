import type { Config } from 'vocs'

export const sidebar = [
  {
    text: 'Start',
    items: [
      { text: 'What is Centaur?', link: '/what-is-centaur' },
      { text: 'Quickstart', link: '/quickstart' },
      { text: 'Deploying in Production', link: '/deploying-in-production' },
      { text: 'Architecture', link: '/architecture' },
    ],
  },
  {
    text: 'Operate',
    items: [
      { text: 'Slack ETL', link: '/operate/slack-etl' },
    ],
  },
  {
    text: 'Extend Centaur',
    items: [
      { text: 'ACME example', link: '/extend/acme-example' },
      { text: 'Using an overlay', link: '/extend/overlay' },
      { text: 'Creating Skills', link: '/extend/skills' },
      { text: 'Creating Tools', link: '/extend/tools' },
      { text: 'Creating Workflows', link: '/extend/workflows' },
      { text: '🚧 Creating Apps', link: '/extend/apps' },
    ],
  },
  {
    text: 'Secrets',
    items: [
      { text: 'How is Centaur securing my secrets?', link: '/security' },
      { text: '1Password', link: '/secrets/onepassword' },
      { text: 'Environment Variables', link: '/secrets/environment' },
      { text: '🚧 AWS KMS', disabled: true },
      { text: '🚧 GCP Secret Manager', disabled: true },
      { text: '🚧 Advanced Permissioning', link: '/secrets/advanced-permissioning' },
    ],
  },
  {
    text: 'Reference',
    items: [
      { text: 'Configuration', link: '/reference/configuration' },
    ],
  },
  {
    text: 'Resources',
    items: [
      { text: 'Brand', link: '/brand' },
      {
        text: 'MIT License',
        link: 'https://github.com/paradigmxyz/centaur/blob/main/LICENSE',
      },
    ],
  },
] satisfies Config['sidebar']
