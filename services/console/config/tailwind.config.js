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
        // Centaur brand accent (green), centered on #28c26a from centaur.run.
        centaur: {
          50: '#e8faf0', 100: '#c6f3da', 200: '#93e7b7',
          300: '#5cd793', 400: '#3ace79', 500: '#28c26a',
          600: '#1ea358', 700: '#1a8147', 800: '#18653a', 900: '#155330'
        },
        // Near-black neutral surfaces matching centaur.run (#050506 page,
        // #101012 / #111114 surfaces, #17171a sunk).
        ink: {
          950: '#050506', 900: '#070708', 850: '#0b0b0d', 800: '#101012',
          700: '#17171a', 600: '#242427', 500: '#33333a'
        }
      },
      fontFamily: {
        mono: [
          'Berkeley Mono',
          'Berkeley Mono Variable',
          'BerkeleyMono',
          'JetBrains Mono',
          'ui-monospace',
          'SFMono-Regular',
          'Menlo',
          'monospace'
        ]
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
