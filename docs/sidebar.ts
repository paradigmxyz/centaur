import type { Config } from 'vocs'

export const sidebar = [
  {
    text: 'Start',
    items: [
      { text: 'What is Centaur?', link: '/what-is-centaur' },
      { text: 'Quickstart', link: '/quickstart' },
      { text: 'Deploying in Production', link: '/deploying-in-production' },
      { text: 'Architecture', link: '/architecture' },
      { text: 'Security', link: '/security' },
    ],
  },
  {
    text: 'Extend Centaur',
    items: [
      { text: 'Using an overlay', link: '/extend/overlay' },
      { text: 'Creating Tools', link: '/extend/tools' },
      { text: 'Creating Workflows', link: '/extend/workflows' },
      { text: 'Creating Skills', link: '/extend/skills' },
    ],
  },
  {
    text: 'Secrets',
    items: [
      { text: 'Use 1Password', link: '/secrets/onepassword' },
      { text: 'Use Environment Variables', link: '/secrets/environment' },
    ],
  },
] satisfies Config['sidebar']
