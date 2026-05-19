/*
 * centaur-brand-menu.js
 *
 * Attaches a right-click (contextmenu) handler to the Centaur logo in the
 * site header. The menu offers three actions: download the brand asset zip,
 * jump to the brand guidelines page, and view the repo. Loaded via vocs.config
 * `head` entry.
 *
 * No deps. Uses a MutationObserver because Vocs rewrites the header on
 * client-side route changes, so the listener may need re-attaching.
 */

;(function () {
  if (typeof window === 'undefined') return

  const MENU_ID = 'centaur-brand-menu'
  const ITEMS = [
    {
      label: 'Download brand assets (.zip)',
      href: '/centaur-brand-assets.zip',
      download: true,
    },
    { label: 'Brand guidelines', href: '/brand' },
    {
      label: 'View on GitHub',
      href: 'https://github.com/paradigmxyz/centaur',
      external: true,
    },
  ]

  function ensureMenuElement() {
    let menu = document.getElementById(MENU_ID)
    if (menu) return menu
    menu = document.createElement('div')
    menu.id = MENU_ID
    menu.className = 'centaur-brand-menu'
    menu.setAttribute('role', 'menu')
    menu.hidden = true
    for (const item of ITEMS) {
      const a = document.createElement('a')
      a.href = item.href
      a.textContent = item.label
      a.setAttribute('role', 'menuitem')
      if (item.download) a.setAttribute('download', '')
      if (item.external) {
        a.target = '_blank'
        a.rel = 'noopener noreferrer'
      }
      a.addEventListener('click', () => {
        hideMenu()
      })
      menu.appendChild(a)
    }
    document.body.appendChild(menu)
    return menu
  }

  function showMenu(x, y) {
    const menu = ensureMenuElement()
    menu.hidden = false
    // Position with a small offset; clamp inside viewport.
    const rect = menu.getBoundingClientRect()
    const maxX = window.innerWidth - rect.width - 8
    const maxY = window.innerHeight - rect.height - 8
    menu.style.left = Math.min(x, maxX) + 'px'
    menu.style.top = Math.min(y, maxY) + 'px'
  }

  function hideMenu() {
    const menu = document.getElementById(MENU_ID)
    if (menu) menu.hidden = true
  }

  function attachToLogo(el) {
    if (!el || el.dataset.centaurBrandMenuBound === '1') return
    el.dataset.centaurBrandMenuBound = '1'
    el.addEventListener('contextmenu', (event) => {
      event.preventDefault()
      showMenu(event.clientX, event.clientY)
    })
  }

  function selectLogos() {
    // Vocs renders the logo as <a href="/"><img ...></a> in the topnav.
    // Match any anchor whose direct child is an img with src containing
    // "/brand/" so we cover both the lockup logo and any future variants.
    const links = document.querySelectorAll('header a, [class*="TopNav"] a')
    const results = []
    links.forEach((a) => {
      const img = a.querySelector('img')
      if (!img) return
      if (img.src.includes('/brand/') || img.alt === 'Centaur') {
        results.push(a)
      }
    })
    return results
  }

  function bind() {
    selectLogos().forEach(attachToLogo)
  }

  // Initial bind + re-bind whenever the DOM mutates (covers route changes).
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', bind, { once: true })
  } else {
    bind()
  }

  const observer = new MutationObserver(() => bind())
  observer.observe(document.body, { childList: true, subtree: true })

  // Dismiss on click outside / escape.
  document.addEventListener('click', (event) => {
    const menu = document.getElementById(MENU_ID)
    if (!menu || menu.hidden) return
    if (!menu.contains(event.target)) hideMenu()
  })
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') hideMenu()
  })
})()
