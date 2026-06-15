module.exports = {
  content: [
    './public/*.html',
    './app/helpers/**/*.rb',
    './app/javascript/**/*.js',
    './app/views/**/*.{erb,haml,html,slim}'
  ],
  theme: {
    extend: {
      colors: {
        iron: {
          50: '#fff4ed', 100: '#ffe6d5', 200: '#ffc9aa',
          300: '#ffa474', 400: '#ff6b35', 500: '#ff5b04',
          600: '#f03f00', 700: '#c72e08', 800: '#9e270f', 900: '#7f2410'
        },
        ink: {
          950: '#0c0d0f', 900: '#08090c', 850: '#0b0d12', 800: '#0f1219',
          700: '#161a24', 600: '#1f2430', 500: '#2b313f'
        }
      },
      fontFamily: {
        mono: ['JetBrains Mono', 'ui-monospace', 'SFMono-Regular', 'Menlo', 'monospace']
      }
    },
    // Very small radii everywhere for the sharp, terminal-ish look.
    borderRadius: {
      none: '0px', sm: '1px', DEFAULT: '2px', md: '2px',
      lg: '2px', xl: '2px', '2xl': '3px', '3xl': '3px', full: '2px'
    }
  },
  plugins: []
}
